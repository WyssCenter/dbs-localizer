// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

pub const LEAD_TUTOR_PATIENT_KEYS: [&str; 10] = [
    "pt_15454", "pt_29781", "pt_33544", "pt_39468", "pt_57245", "pt_76325", "pt_78754", "pt_80206",
    "pt_84257", "pt_93127",
];

pub const DEFAULT_POST_URL: &str = "http://tutor.lead-dbs.org:8000/my-endpoint";
pub const DEFAULT_DASHBOARD_BASE: &str = "http://tutor.lead-dbs.org/?userid=";
pub const USER_ID_ENV: &str = "LEAD_TUTOR_USER_ID";
const SAVED_USER_ID_FILE: &str = "lead_tutor.json";
pub const REFERENCE_SPACE: &str = "MNI152NLin2009bAsym";
pub const SIDE_KEYS_CONVENTION: &str = "1=right,2=left";
pub const PROBE_HINTS: &str = "To discover FastAPI routes locally (don't hammer the server), \
try once each:\n  curl -sI https://tutor.lead-dbs.org/docs\n  curl -sI \
https://tutor.lead-dbs.org/openapi.json\n  curl -sI http://tutor.lead-dbs.org:8000/docs\n  \
curl -sI http://tutor.lead-dbs.org:8000/openapi.json\nCommon FastAPI: /docs, /redoc, \
/openapi.json — availability depends on deploy.";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const SIDES: [&str; 2] = ["leftElectrode", "rightElectrode"];
const ELECTRODE_ATTRS: [&str; 3] = ["head", "tail", "x"];

pub fn default_post_url() -> String {
    std::env::var("LEAD_TUTOR_POST_URL").unwrap_or_else(|_| DEFAULT_POST_URL.to_owned())
}

pub fn default_dashboard_base() -> String {
    std::env::var("LEAD_TUTOR_DASHBOARD_URL").unwrap_or_else(|_| DEFAULT_DASHBOARD_BASE.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserIdSource {
    Flag,
    Env,
    Saved(PathBuf),
}

pub fn saved_user_id_path() -> Option<PathBuf> {
    crate::template_catalog::user_data_dir().map(|d| d.join(SAVED_USER_ID_FILE))
}

pub fn read_saved_user_id(path: &Path) -> Result<Option<String>, String> {
    let hint = "delete it or run `dbsloc tutor user-id --set <id>`";
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "cannot read the saved Lead-Tutor user id file {}: {e}; {hint}",
                path.display()
            ))
        }
    };
    let value: Value = serde_json::from_slice(&raw).map_err(|e| {
        format!(
            "the saved Lead-Tutor user id file {} is not valid JSON ({e}); {hint}",
            path.display()
        )
    })?;
    let Some(id) = value.get("user_id").and_then(Value::as_str) else {
        return Err(format!(
            "the saved Lead-Tutor user id file {} has no \"user_id\" text value; {hint}",
            path.display()
        ));
    };
    check_user_id(id).map(Some).map_err(|e| {
        format!(
            "the saved Lead-Tutor user id file {} is unusable ({e}); {hint}",
            path.display()
        )
    })
}

pub fn save_user_id(path: &Path, id: &str) -> Result<(), String> {
    let id = check_user_id(id)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut text =
        serde_json::to_string_pretty(&json!({ "user_id": id })).map_err(|e| e.to_string())?;
    text.push('\n');
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(SAVED_USER_ID_FILE);
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {}: {e}", path.display())
    })
}

pub fn generate_user_id() -> String {
    let key = crate::reports::random_key();
    let hex: String = key[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("dbsloc-{hex}")
}

pub fn configured_user_id_with(
    flag: Option<&str>,
    env: Option<&str>,
    saved: Option<&Path>,
) -> Result<Option<(String, UserIdSource)>, String> {
    if let Some(raw) = flag {
        return check_user_id(raw).map(|id| Some((id, UserIdSource::Flag)));
    }
    if let Some(raw) = env.map(str::trim).filter(|v| !v.is_empty()) {
        return check_user_id(raw)
            .map(|id| Some((id, UserIdSource::Env)))
            .map_err(|e| format!("{USER_ID_ENV}: {e}"));
    }
    match saved {
        Some(path) => {
            Ok(read_saved_user_id(path)?.map(|id| (id, UserIdSource::Saved(path.to_path_buf()))))
        }
        None => Ok(None),
    }
}

pub fn configured_user_id(flag: Option<&str>) -> Result<Option<(String, UserIdSource)>, String> {
    let env = std::env::var(USER_ID_ENV).ok();
    let path = saved_user_id_path();
    configured_user_id_with(flag, env.as_deref(), path.as_deref())
}

pub fn check_user_id(raw: &str) -> Result<String, String> {
    let id = raw.trim();
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if id.is_empty() || !id.chars().all(safe) {
        return Err(format!(
            "invalid User-ID {raw:?}: use only letters, digits, '.', '_' and '-'"
        ));
    }
    Ok(id.to_owned())
}

pub fn dashboard_url(user_id: &str) -> String {
    format!("{}{user_id}", default_dashboard_base())
}

pub fn example_payload() -> Value {
    json!({
        "pt_15454": {
            "leftElectrode": {
                "head": [0.0, 0.0, 0.0],
                "tail": [0.0, 0.0, -6.0],
                "x": [1.0, 0.0, 0.0],
            },
            "rightElectrode": {
                "head": [1.0, 0.0, 0.0],
                "tail": [1.0, 0.0, -6.0],
                "x": [0.0, 1.0, 0.0],
            },
        }
    })
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn py_str_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", py_str_repr(k), py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn py_str_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| py_str_repr(s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn triplet_errors(label: &str, value: &Value) -> Vec<String> {
    let Value::Array(items) = value else {
        return vec![format!(
            "{label}: expected array of 3 numbers, got {}",
            type_name(value)
        )];
    };
    if items.len() != 3 {
        return vec![format!("{label}: expected 3 elements, got {}", items.len())];
    }
    items
        .iter()
        .enumerate()
        .filter(|(_, x)| !x.is_number())
        .map(|(i, x)| format!("{label}[{i}]: not a number ({})", py_repr(x)))
        .collect()
}

fn electrode_errors(prefix: &str, side: &Value) -> Vec<String> {
    let Value::Object(map) = side else {
        return vec![format!("{prefix}: must be object, not {}", type_name(side))];
    };
    let mut errs: Vec<String> = map
        .keys()
        .filter(|k| !ELECTRODE_ATTRS.contains(&k.as_str()))
        .map(|k| format!("{prefix}: unknown field {}", py_str_repr(k)))
        .collect();
    for attr in ELECTRODE_ATTRS {
        match map.get(attr) {
            None => errs.push(format!("{prefix}: missing '{attr}'")),
            Some(v) => errs.extend(triplet_errors(&format!("{prefix}.{attr}"), v)),
        }
    }
    errs
}

pub fn validate_tutor_markers(obj: &Value) -> Vec<String> {
    let Value::Object(root) = obj else {
        return vec![format!(
            "root must be a JSON object (dict), not {}",
            type_name(obj)
        )];
    };
    let mut errs = Vec::new();
    for (key, patient) in root {
        if !LEAD_TUTOR_PATIENT_KEYS.contains(&key.as_str()) {
            errs.push(format!("unknown patient key: {}", py_str_repr(key)));
            continue;
        }
        let Value::Object(pat) = patient else {
            errs.push(format!(
                "{key}: value must be object, not {}",
                type_name(patient)
            ));
            continue;
        };
        errs.extend(
            pat.keys()
                .filter(|k| !SIDES.contains(&k.as_str()))
                .map(|k| format!("{key}: unknown field {}", py_str_repr(k))),
        );
        for side in SIDES {
            match pat.get(side) {
                None => errs.push(format!("{key}: missing '{side}'")),
                Some(v) => errs.extend(electrode_errors(&format!("{key}.{side}"), v)),
            }
        }
    }
    errs
}

pub fn body_from_value(obj: &Value) -> Result<Vec<u8>, String> {
    if !obj.is_object() {
        return Err("JSON root must be an object (dict of pt_* -> electrodes)".to_owned());
    }
    let errs = validate_tutor_markers(obj);
    if !errs.is_empty() {
        return Err(format!(
            "Lead-Tutor marker validation failed:\n{}",
            errs.join("\n")
        ));
    }
    serde_json::to_vec(obj).map_err(|e| e.to_string())
}

pub fn read_post_body(path: &Path) -> Result<Vec<u8>, String> {
    let raw = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;
    let obj: Value = serde_json::from_slice(&raw).map_err(|e| format!("invalid JSON: {e}"))?;
    body_from_value(&obj)
}

pub fn patient_key(sub: &str) -> Result<String, String> {
    let s = sub.trim();
    if s.is_empty() {
        return Err("empty subject id; expected sub-<id>, <id>, or pt_<id>".to_owned());
    }
    let digits = |rest: &str| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    let key = if let Some(rest) = s.strip_prefix("pt_") {
        if !digits(rest) {
            return Err(format!(
                "invalid pt_ id (digits only after pt_): {}",
                py_str_repr(sub)
            ));
        }
        format!("pt_{rest}")
    } else if let Some(rest) = s.strip_prefix("sub-") {
        if !digits(rest) {
            return Err(format!("expected sub-<digits>, got: {}", py_str_repr(sub)));
        }
        format!("pt_{rest}")
    } else if let Some(rest) = s.strip_prefix("sub_") {
        if !digits(rest) {
            return Err(format!("expected sub_<digits>, got: {}", py_str_repr(sub)));
        }
        format!("pt_{rest}")
    } else if digits(s) {
        format!("pt_{s}")
    } else {
        return Err(format!(
            "unrecognized subject id: {}; use sub-<id>, <id>, or pt_<id> (Lead-Tutor ids: {})",
            py_str_repr(sub),
            LEAD_TUTOR_PATIENT_KEYS.join(", ")
        ));
    };
    if !LEAD_TUTOR_PATIENT_KEYS.contains(&key.as_str()) {
        return Err(format!(
            "unknown Lead-Tutor patient {}; allowed: {}",
            py_str_repr(&key),
            LEAD_TUTOR_PATIENT_KEYS.join(", ")
        ));
    }
    Ok(key)
}

#[derive(Debug, Clone)]
pub struct RecoV2 {
    pub subj_id: String,
    pub method: String,
    pub mni: Map<String, Value>,
    pub meta: Map<String, Value>,
    pub reference: Option<Map<String, Value>>,
}

impl RecoV2 {
    pub fn from_value(d: &Value) -> Result<RecoV2, String> {
        let Value::Object(map) = d else {
            return Err("reconstruction JSON root must be an object".to_owned());
        };
        let version = map.get("version").and_then(Value::as_f64).unwrap_or(1.0);
        if (version - 2.0).abs() > f64::EPSILON {
            return Err("RecoFileV2: expected version 2".to_owned());
        }
        let text = |k: &str| match map.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        let object = |k: &str| match map.get(k) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let reference = match map.get("reference") {
            Some(Value::Object(o)) if !o.is_empty() => Some(o.clone()),
            _ => None,
        };
        Ok(RecoV2 {
            subj_id: text("subj_id"),
            method: text("method"),
            mni: object("mni"),
            meta: object("meta"),
            reference,
        })
    }

    pub fn read(path: &Path) -> Result<RecoV2, String> {
        let raw = std::fs::read(path).map_err(|e| e.to_string())?;
        let mut v: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        for c in dbsloc_reconstruct::tail_fiducial::convert_legacy_tail(&mut v, None) {
            eprintln!("{}: {}", path.display(), c.warning);
        }
        RecoV2::from_value(&v)
    }
}

fn number_of(v: &Value) -> Result<f64, String> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .ok_or_else(|| format!("not a number: {}", py_repr(v)))
}

fn side_mean_x(side: &Value) -> Result<Option<f64>, String> {
    let Value::Object(map) = side else {
        return Ok(None);
    };
    if let Some(Value::Object(mk)) = map.get("markers") {
        if let (Some(h), Some(t)) = (mk.get("head"), mk.get("tail")) {
            let hx = h.get(0).ok_or("markers.head is empty")?;
            let tx = t.get(0).ok_or("markers.tail is empty")?;
            return Ok(Some(0.5 * (number_of(hx)? + number_of(tx)?)));
        }
    }
    for k in ["trajectory", "coords_mm"] {
        if let Some(Value::Array(pts)) = map.get(k) {
            if !pts.is_empty() {
                let mut sum = 0.0;
                for p in pts {
                    sum += number_of(p.get(0).ok_or("point is empty")?)?;
                }
                return Ok(Some(sum / pts.len() as f64));
            }
        }
    }
    Ok(None)
}

pub fn side_hemispheres(
    meta: &Map<String, Value>,
    block: &Map<String, Value>,
    space: &str,
) -> Result<Vec<(String, &'static str)>, String> {
    let objects: Vec<(&String, &Value)> = block.iter().filter(|(_, v)| v.is_object()).collect();
    match meta.get("side_keys") {
        Some(Value::Null) | None => {}
        Some(declared) => {
            if declared.as_str() != Some(SIDE_KEYS_CONVENTION) {
                return Err(format!(
                    "meta.side_keys {} is not a known convention",
                    py_repr(declared)
                ));
            }
            return Ok(objects
                .iter()
                .filter_map(|(k, _)| match k.as_str() {
                    "1" => Some(((*k).clone(), "right")),
                    "2" => Some(((*k).clone(), "left")),
                    _ => None,
                })
                .collect());
        }
    }
    let mut xs: Vec<(String, f64)> = Vec::new();
    for (k, v) in &objects {
        if let Some(x) = side_mean_x(v)? {
            xs.push(((*k).clone(), x));
        }
    }
    match xs.as_slice() {
        [] => Ok(Vec::new()),
        [(k, x)] => Ok(vec![(k.clone(), if *x > 0.0 { "right" } else { "left" })]),
        [(ka, xa), (kb, xb)] => {
            if xa == xb {
                return Err(format!(
                    "both {space} sides have the same mean x ({xa:.2} mm); cannot tell the \
                     hemispheres apart"
                ));
            }
            if xa > xb {
                Ok(vec![(ka.clone(), "right"), (kb.clone(), "left")])
            } else {
                Ok(vec![(ka.clone(), "left"), (kb.clone(), "right")])
            }
        }
        more => Err(format!(
            "expected at most two sides in {space}, found {}",
            more.len()
        )),
    }
}

fn triplet(v: &Value, what: &str) -> Result<[f64; 3], String> {
    let arr = v
        .as_array()
        .filter(|a| a.len() >= 3)
        .ok_or_else(|| format!("markers.{what} must hold three numbers"))?;
    let mut out = [0.0; 3];
    for (slot, item) in out.iter_mut().zip(arr) {
        *slot = number_of(item).map_err(|e| format!("markers.{what}: {e}"))?;
    }
    Ok(out)
}

fn electrode_json(markers: &Value) -> Result<Value, String> {
    let get = |k: &str| {
        markers
            .get(k)
            .ok_or_else(|| format!("markers has no {k:?}"))
            .and_then(|v| triplet(v, k))
    };
    Ok(json!({
        "head": get("head")?,
        "tail": get("tail")?,
        "x": get("x")?,
    }))
}

pub fn payload_from_reco(sub_bids: &str, reco: &RecoV2) -> Result<Value, String> {
    let space = reco
        .meta
        .get("template_space")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut block_name = "mni";
    let mut mni: Map<String, Value> = reco.mni.clone();
    if let Some(space) = space.filter(|s| *s != REFERENCE_SPACE) {
        let Some(reference) = &reco.reference else {
            return Err(format!(
                "reconstruction is in {space}; Lead-Tutor needs {REFERENCE_SPACE} coordinates \
                 and the file has no 'reference' block — re-run reconstruct"
            ));
        };
        block_name = "reference";
        mni = reference
            .iter()
            .filter(|(_, v)| v.is_object())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
    }
    if mni.is_empty() {
        return Err(format!(
            "reconstruction for {} has no MNI block (method={}); Lead-Tutor compares markers in \
             MNI space. Rerun reconstruct with a template and the normalize transforms so the \
             native points are warped to it.",
            py_str_repr(sub_bids),
            py_str_repr(&reco.method)
        ));
    }
    let missing: Vec<String> = ["1", "2"]
        .iter()
        .filter(|k| !mni.contains_key(**k))
        .map(|k| (*k).to_owned())
        .collect();
    if !missing.is_empty() {
        let mut have: Vec<String> = mni.keys().cloned().collect();
        have.sort();
        return Err(format!(
            "reconstruction for {} is missing MNI side(s) {} (have {}); Lead-Tutor needs both \
             hemispheres.",
            py_str_repr(sub_bids),
            py_str_list(&missing),
            py_str_list(&have)
        ));
    }
    let hemi = side_hemispheres(&reco.meta, &mni, block_name)?;
    let key_of = |want: &str| {
        hemi.iter()
            .rev()
            .find(|(_, h)| *h == want)
            .map(|(k, _)| k.clone())
    };
    let (Some(right_key), Some(left_key)) = (key_of("right"), key_of("left")) else {
        let shown = hemi
            .iter()
            .map(|(k, h)| format!("{}: '{h}'", py_str_repr(k)))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "reconstruction for {} does not resolve to one right and one left lead (have \
             {{{shown}}}); Lead-Tutor needs both hemispheres.",
            py_str_repr(sub_bids)
        ));
    };
    let markers = |k: &str| {
        mni.get(k)
            .and_then(|s| s.get("markers"))
            .ok_or_else(|| format!("MNI side {k:?} has no 'markers'"))
            .and_then(electrode_json)
    };
    let positive_x = markers(&right_key)?;
    let negative_x = markers(&left_key)?;
    let key = patient_key(sub_bids)?;
    let mut patient = Map::new();
    patient.insert("leftElectrode".to_owned(), positive_x);
    patient.insert("rightElectrode".to_owned(), negative_x);
    let mut out = Map::new();
    out.insert(key, Value::Object(patient));
    Ok(Value::Object(out))
}

#[derive(Debug)]
pub enum TutorError {
    NotFound(String),
    Invalid(String),
}

impl std::fmt::Display for TutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TutorError::NotFound(m) | TutorError::Invalid(m) => f.write_str(m),
        }
    }
}

pub fn payload_from_recon_json(path: &Path) -> Result<Value, TutorError> {
    let p = crate::paths::absolute(path);
    if !p.is_file() {
        return Err(TutorError::NotFound(format!(
            "reconstruction JSON not found: {}",
            p.display()
        )));
    }
    let reco = RecoV2::read(&p).map_err(TutorError::Invalid)?;
    payload_from_reco(&reco.subj_id, &reco).map_err(TutorError::Invalid)
}

#[derive(Debug, Clone)]
pub struct MergedPayload {
    pub payload: Value,
    pub skipped: Vec<String>,
}

pub fn merge_payloads(dataset_root: &Path, require_all: bool) -> Result<MergedPayload, TutorError> {
    merge_payloads_in(dataset_root, None, require_all)
}

/// Merge the reconstructions of every subject under the output root
/// [`crate::paths::derivatives_root`]`(dataset_root, output_dir)`.
pub fn merge_payloads_in(
    dataset_root: &Path,
    output_dir: Option<&Path>,
    require_all: bool,
) -> Result<MergedPayload, TutorError> {
    let dlb = crate::paths::derivatives_root(&crate::paths::absolute(dataset_root), output_dir);
    if !dlb.is_dir() {
        return Err(TutorError::NotFound(format!(
            "not a directory: {}",
            dlb.display()
        )));
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&dlb)
        .map_err(|e| TutorError::Invalid(e.to_string()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("sub-"))
        })
        .collect();
    dirs.sort();
    let mut merged = Map::new();
    let mut skipped = Vec::new();
    for dir in dirs {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let subj = name.get(4..).unwrap_or_default();
        let rpath = dir
            .join("reconstruction")
            .join(format!("sub-{subj}_desc-reconstruction.json"));
        if !rpath.is_file() {
            continue;
        }
        if let Err(e) = patient_key(subj) {
            skipped.push(format!("{name}: skipped, not a Lead-Tutor subject ({e})"));
            continue;
        }
        let Value::Object(part) = payload_from_recon_json(&rpath)? else {
            continue;
        };
        for (k, v) in part {
            if merged.contains_key(&k) {
                return Err(TutorError::Invalid(format!(
                    "duplicate Lead-Tutor key: {}",
                    py_str_repr(&k)
                )));
            }
            merged.insert(k, v);
        }
    }
    if require_all {
        for need in LEAD_TUTOR_PATIENT_KEYS {
            if !merged.contains_key(need) {
                return Err(TutorError::Invalid(format!(
                    "missing Lead-Tutor patient {} under {} (require_keys)",
                    py_str_repr(need),
                    dlb.display()
                )));
            }
        }
    }
    Ok(MergedPayload {
        payload: Value::Object(merged),
        skipped,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusAndText {
    pub status: u16,
    pub reason: String,
    pub text: String,
}

impl StatusAndText {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn post_payload(url: &str, user_id: &str, body: &[u8]) -> Result<StatusAndText, String> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(REQUEST_TIMEOUT))
        .timeout_send_request(Some(REQUEST_TIMEOUT))
        .timeout_send_body(Some(REQUEST_TIMEOUT))
        .timeout_recv_response(Some(REQUEST_TIMEOUT))
        .timeout_recv_body(Some(REQUEST_TIMEOUT))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-ID", user_id)
        .send(body)
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let raw = response
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    Ok(StatusAndText {
        status: status.as_u16(),
        reason: status.canonical_reason().unwrap_or("").to_owned(),
        text: String::from_utf8_lossy(&raw).into_owned(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn electrode() -> Value {
        json!({"head": [0.0, 0.0, 0.0], "tail": [0.0, 0.0, -6.0], "x": [1.0, 0.0, 0.0]})
    }

    fn patient() -> Value {
        json!({"leftElectrode": electrode(), "rightElectrode": electrode()})
    }

    fn side(x: f64) -> Value {
        json!({"markers": {
            "head": [x, -8.0, -9.0],
            "tail": [x, -3.0, -1.0],
            "x": [x + 0.65, -8.0, -9.0],
            "y": [x, -7.35, -9.0],
        }})
    }

    fn side_shifted(x: f64, shift: f64) -> Value {
        json!({"markers": {
            "head": [x + shift, 1.0, 2.0],
            "tail": [x + shift, 1.0, -4.0],
            "x": [x + shift + 1.0, 1.0, 2.0],
        }})
    }

    fn reco_value(subj: &str, mni: Value, meta: Value, reference: Value) -> Value {
        let mut v = json!({"version": 2, "subj_id": subj, "method": "test", "mni": mni});
        if !meta.is_null() {
            v["meta"] = meta;
        }
        if !reference.is_null() {
            v["reference"] = reference;
        }
        v
    }

    fn payload_of(v: &Value) -> Result<Value, String> {
        let reco = RecoV2::from_value(v)?;
        payload_from_reco(&reco.subj_id.clone(), &reco)
    }

    #[test]
    fn patient_keys_match_upstream_order() {
        assert_eq!(
            LEAD_TUTOR_PATIENT_KEYS,
            [
                "pt_15454", "pt_29781", "pt_33544", "pt_39468", "pt_57245", "pt_76325", "pt_78754",
                "pt_80206", "pt_84257", "pt_93127"
            ]
        );
    }

    #[test]
    fn validate_accepts_the_documented_shapes() {
        assert!(validate_tutor_markers(&json!({})).is_empty());
        assert!(validate_tutor_markers(&json!({"pt_15454": patient()})).is_empty());
        let all: Map<String, Value> = LEAD_TUTOR_PATIENT_KEYS
            .iter()
            .map(|k| ((*k).to_owned(), patient()))
            .collect();
        assert!(validate_tutor_markers(&Value::Object(all)).is_empty());
    }

    #[test]
    fn validate_rejects_strings_booleans_and_extra_fields() {
        let strings = json!({"head": ["0", "1", "2"], "tail": [0, 0, -6], "x": [true, 0, 0]});
        let errs = validate_tutor_markers(
            &json!({"pt_29781": {"leftElectrode": strings, "rightElectrode": electrode()}}),
        );
        assert_eq!(
            errs,
            vec![
                "pt_29781.leftElectrode.head[0]: not a number ('0')",
                "pt_29781.leftElectrode.head[1]: not a number ('1')",
                "pt_29781.leftElectrode.head[2]: not a number ('2')",
                "pt_29781.leftElectrode.x[0]: not a number (True)",
            ]
        );
        let mut extra = electrode();
        extra["name"] = json!("Patient Name");
        let errs = validate_tutor_markers(&json!({"pt_29781": {
            "leftElectrode": extra,
            "rightElectrode": electrode(),
            "note": "x",
        }}));
        assert_eq!(
            errs,
            vec![
                "pt_29781: unknown field 'note'",
                "pt_29781.leftElectrode: unknown field 'name'",
            ]
        );
        let err = body_from_value(&json!({"pt_29781": {
            "leftElectrode": electrode(),
            "rightElectrode": electrode(),
            "dicom": "x",
        }}))
        .unwrap_err();
        assert!(err.contains("unknown field 'dicom'"), "{err}");
    }

    #[test]
    fn validate_reports_python_messages() {
        assert_eq!(
            validate_tutor_markers(&json!([])),
            vec!["root must be a JSON object (dict), not list"]
        );
        assert_eq!(
            validate_tutor_markers(&Value::Null),
            vec!["root must be a JSON object (dict), not NoneType"]
        );
        assert_eq!(
            validate_tutor_markers(&json!({"pt_99999": patient()})),
            vec!["unknown patient key: 'pt_99999'"]
        );
        assert_eq!(
            validate_tutor_markers(&json!({"pt_15454": []})),
            vec!["pt_15454: value must be object, not list"]
        );
        let errs = validate_tutor_markers(&json!({"pt_15454": {"leftElectrode": electrode()}}));
        assert!(errs.contains(&"pt_15454: missing 'rightElectrode'".to_owned()));
        let bad = json!({"head": [0, 0, 0], "tail": [0, 0, 0]});
        let errs = validate_tutor_markers(
            &json!({"pt_15454": {"leftElectrode": bad, "rightElectrode": electrode()}}),
        );
        assert!(errs.iter().any(|m| m.contains("missing 'x'")));
        let bad = json!({"head": [0, 0], "tail": [0, 0, 0], "x": [1, 0, 0]});
        let errs = validate_tutor_markers(
            &json!({"pt_15454": {"leftElectrode": bad, "rightElectrode": electrode()}}),
        );
        assert!(errs.iter().any(|m| m.contains("expected 3 elements")));
        let bad = json!({"head": "nope", "tail": [0, 0, 0], "x": [1, 0, 0]});
        let errs = validate_tutor_markers(
            &json!({"pt_15454": {"leftElectrode": bad, "rightElectrode": electrode()}}),
        );
        assert!(errs
            .iter()
            .any(|m| m.contains("expected array of 3 numbers, got str")));
        let errs = validate_tutor_markers(
            &json!({"pt_15454": {"leftElectrode": [], "rightElectrode": electrode()}}),
        );
        assert!(errs
            .iter()
            .any(|m| m.contains("pt_15454.leftElectrode: must be object")));
        let bad = json!({"head": [0, "a", null], "tail": [0, 0, 0], "x": [1, 0, 0]});
        let errs = validate_tutor_markers(
            &json!({"pt_15454": {"leftElectrode": bad, "rightElectrode": electrode()}}),
        );
        assert_eq!(
            errs,
            vec![
                "pt_15454.leftElectrode.head[1]: not a number ('a')",
                "pt_15454.leftElectrode.head[2]: not a number (None)"
            ]
        );
    }

    #[test]
    fn body_is_compact_json_and_incomplete_is_refused() {
        let body = body_from_value(&json!({"pt_15454": patient()})).unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(!text.contains(' '));
        assert!(text.starts_with("{\"pt_15454\":{\"leftElectrode\":{\"head\":[0.0,0.0,0.0]"));
        let err = body_from_value(&json!({"pt_15454": {}})).unwrap_err();
        assert!(err.contains("validation failed"), "{err}");
        assert!(body_from_value(&json!([])).is_err());
    }

    #[test]
    fn read_post_body_reports_unreadable_and_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = read_post_body(&dir.path().join("none.json")).unwrap_err();
        assert!(missing.starts_with("cannot read: "), "{missing}");
        let p = dir.path().join("bad.json");
        std::fs::write(&p, b"{").unwrap();
        assert!(read_post_body(&p)
            .unwrap_err()
            .starts_with("invalid JSON: "));
        std::fs::write(&p, b"[]").unwrap();
        assert!(read_post_body(&p)
            .unwrap_err()
            .contains("must be an object"));
        std::fs::write(&p, b"{\"pt_15454\":{}}").unwrap();
        assert!(read_post_body(&p)
            .unwrap_err()
            .contains("validation failed"));
    }

    #[test]
    fn subject_ids_map_to_patient_keys() {
        assert_eq!(patient_key("15454").unwrap(), "pt_15454");
        assert_eq!(patient_key("sub-15454").unwrap(), "pt_15454");
        assert_eq!(patient_key("sub_15454").unwrap(), "pt_15454");
        assert_eq!(patient_key("pt_15454").unwrap(), "pt_15454");
        assert_eq!(patient_key("  sub-15454  ").unwrap(), "pt_15454");
        assert!(patient_key("99999")
            .unwrap_err()
            .contains("unknown Lead-Tutor patient"));
        assert!(patient_key("").unwrap_err().contains("empty subject id"));
        assert!(patient_key("sub-x")
            .unwrap_err()
            .contains("expected sub-<digits>"));
        assert!(patient_key("pt_x").unwrap_err().contains("invalid pt_ id"));
        assert!(patient_key("abc")
            .unwrap_err()
            .contains("unrecognized subject id"));
    }

    #[test]
    fn left_field_carries_the_positive_x_lead_for_either_key_order() {
        let (pos, neg) = (22.9, -21.8);
        for (k_pos, k_neg) in [("1", "2"), ("2", "1")] {
            let mut mni = Map::new();
            mni.insert(k_pos.to_owned(), side(pos));
            mni.insert(k_neg.to_owned(), side(neg));
            let v = reco_value("76325", Value::Object(mni), Value::Null, Value::Null);
            let p = payload_of(&v).unwrap();
            let pt = &p["pt_76325"];
            assert_eq!(pt["leftElectrode"]["head"][0], json!(pos));
            assert_eq!(pt["rightElectrode"]["head"][0], json!(neg));
            let keys: Vec<&String> = pt.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["leftElectrode", "rightElectrode"]);
            let inner: Vec<&String> = pt["leftElectrode"].as_object().unwrap().keys().collect();
            assert_eq!(inner, ["head", "tail", "x"]);
        }
    }

    #[test]
    fn declared_side_keys_are_read_by_key() {
        let v = reco_value(
            "76325",
            json!({"1": side(-5.0), "2": side(7.0)}),
            json!({"side_keys": "1=right,2=left"}),
            Value::Null,
        );
        let p = payload_of(&v).unwrap();
        assert_eq!(p["pt_76325"]["leftElectrode"]["head"][0], json!(-5.0));
        assert_eq!(p["pt_76325"]["rightElectrode"]["head"][0], json!(7.0));
        let v = reco_value(
            "76325",
            json!({"1": side(-5.0), "2": side(7.0)}),
            json!({"side_keys": "1=left,2=right"}),
            Value::Null,
        );
        assert_eq!(
            payload_of(&v).unwrap_err(),
            "meta.side_keys '1=left,2=right' is not a known convention"
        );
    }

    #[test]
    fn two_leads_at_the_same_x_are_refused() {
        let v = reco_value(
            "76325",
            json!({"1": side(12.0), "2": side(12.0)}),
            Value::Null,
            Value::Null,
        );
        assert!(payload_of(&v).unwrap_err().contains("same mean x"));
    }

    #[test]
    fn a_reconstruction_without_mni_names_the_method() {
        let v = json!({"version": 2, "subj_id": "76325", "method": "pacer_rust", "mni": {}, "native": {"1": {}}});
        let err = payload_of(&v).unwrap_err();
        assert!(err.contains("no MNI block"), "{err}");
        assert!(err.contains("method='pacer_rust'"), "{err}");
    }

    #[test]
    fn a_missing_side_is_refused() {
        let v = reco_value("15454", json!({"1": side(3.0)}), Value::Null, Value::Null);
        assert_eq!(
            payload_of(&v).unwrap_err(),
            "reconstruction for '15454' is missing MNI side(s) ['2'] (have ['1']); Lead-Tutor \
             needs both hemispheres."
        );
    }

    #[test]
    fn version_one_files_are_refused() {
        let err = RecoV2::from_value(&json!({"subj_id": "15454", "mni": {}})).unwrap_err();
        assert_eq!(err, "RecoFileV2: expected version 2");
    }

    fn reference_reco(space: Option<&str>, with_reference: bool) -> Value {
        let mut meta = json!({"side_keys": "1=right,2=left"});
        if let Some(s) = space {
            meta["template_space"] = json!(s);
        }
        let reference = if with_reference {
            json!({
                "space": "MNI152NLin2009bAsym",
                "1": side_shifted(10.0, 5.0),
                "2": side_shifted(-10.0, 5.0),
            })
        } else {
            Value::Null
        };
        reco_value(
            "15454",
            json!({"1": side_shifted(10.0, 0.0), "2": side_shifted(-10.0, 0.0)}),
            meta,
            reference,
        )
    }

    #[test]
    fn the_reference_space_uses_the_mni_block() {
        for space in [None, Some("MNI152NLin2009bAsym")] {
            let p = payload_of(&reference_reco(space, true)).unwrap();
            assert_eq!(p["pt_15454"]["leftElectrode"]["head"][0], json!(10.0));
            assert_eq!(p["pt_15454"]["rightElectrode"]["head"][0], json!(-10.0));
        }
    }

    #[test]
    fn another_template_uses_the_reference_block() {
        let p = payload_of(&reference_reco(Some("MNI152NLin2009cSym"), true)).unwrap();
        assert_eq!(p["pt_15454"]["leftElectrode"]["head"][0], json!(15.0));
        assert_eq!(p["pt_15454"]["rightElectrode"]["head"][0], json!(-5.0));
    }

    #[test]
    fn another_template_without_a_reference_block_is_refused() {
        assert_eq!(
            payload_of(&reference_reco(Some("MNI152NLin2009cSym"), false)).unwrap_err(),
            "reconstruction is in MNI152NLin2009cSym; Lead-Tutor needs MNI152NLin2009bAsym \
             coordinates and the file has no 'reference' block — re-run reconstruct"
        );
    }

    #[test]
    fn the_reference_block_with_only_a_space_key_has_no_sides() {
        let v = reco_value(
            "15454",
            json!({"1": side(3.0), "2": side(-3.0)}),
            json!({"template_space": "MNI152NLin2009cSym"}),
            json!({"space": "MNI152NLin2009bAsym"}),
        );
        assert!(payload_of(&v).unwrap_err().contains("no MNI block"));
    }

    fn write_reco(dir: &Path, subj: &str, x: f64) -> PathBuf {
        let d = dir
            .join("derivatives/leaddbs")
            .join(format!("sub-{subj}/reconstruction"));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("sub-{subj}_desc-reconstruction.json"));
        let v = reco_value(
            subj,
            json!({"1": side(x), "2": side(-x)}),
            json!({"side_keys": "1=right,2=left"}),
            Value::Null,
        );
        std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        p
    }

    #[test]
    fn file_round_trip_and_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_reco(dir.path(), "15454", 10.0);
        let payload = payload_from_recon_json(&p).unwrap();
        let keys: Vec<&String> = payload.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["pt_15454"]);
        assert!(validate_tutor_markers(&payload).is_empty());
        let body = body_from_value(&payload).unwrap();
        let back: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(back, payload);
        assert!(matches!(
            payload_from_recon_json(&dir.path().join("nope.json")),
            Err(TutorError::NotFound(_))
        ));
    }

    #[test]
    fn merging_collects_every_subject_and_enforces_require_all() {
        let dir = tempfile::tempdir().unwrap();
        write_reco(dir.path(), "15454", 10.0);
        write_reco(dir.path(), "29781", 11.0);
        write_reco(dir.path(), "77777", 12.0);
        let merged = merge_payloads(dir.path(), false).unwrap();
        let keys: Vec<&String> = merged.payload.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["pt_15454", "pt_29781"]);
        assert_eq!(merged.skipped.len(), 1);
        assert!(
            merged.skipped[0].starts_with("sub-77777: skipped, not a Lead-Tutor subject"),
            "{:?}",
            merged.skipped
        );
        let err = merge_payloads(dir.path(), true).unwrap_err().to_string();
        assert!(
            err.contains("missing Lead-Tutor patient 'pt_33544'"),
            "{err}"
        );
        assert!(matches!(
            merge_payloads(&dir.path().join("empty"), false),
            Err(TutorError::NotFound(_))
        ));
    }

    #[test]
    fn user_ids_are_checked() {
        assert_eq!(check_user_id(" 482197 ").unwrap(), "482197");
        assert_eq!(check_user_id("run-2.a_b").unwrap(), "run-2.a_b");
        for bad in ["", "  ", "a b", "x&y=1", "x/y", "é"] {
            assert!(
                check_user_id(bad).unwrap_err().contains("invalid User-ID"),
                "{bad:?}"
            );
        }
        assert_eq!(
            dashboard_url("482197"),
            format!("{}482197", default_dashboard_base())
        );
    }

    fn saved_file(dir: &Path, content: &str) -> PathBuf {
        let p = dir.join("lead_tutor.json");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn user_id_precedence_is_flag_then_env_then_saved() {
        let dir = tempfile::tempdir().unwrap();
        let saved = saved_file(dir.path(), "{\"user_id\": \"from-file\"}");
        let got = |f, e| configured_user_id_with(f, e, Some(&saved)).unwrap();
        assert_eq!(
            got(Some(" flag-1 "), Some("env-1")),
            Some(("flag-1".to_owned(), UserIdSource::Flag))
        );
        assert_eq!(
            got(None, Some("  env-1\t")),
            Some(("env-1".to_owned(), UserIdSource::Env))
        );
        assert_eq!(
            got(None, Some("   ")),
            Some(("from-file".to_owned(), UserIdSource::Saved(saved.clone())))
        );
        assert_eq!(
            got(None, None),
            Some(("from-file".to_owned(), UserIdSource::Saved(saved.clone())))
        );
        assert_eq!(configured_user_id_with(None, None, None).unwrap(), None);
    }

    #[test]
    fn invalid_ids_and_a_broken_saved_file_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(configured_user_id_with(Some("a b"), None, None)
            .unwrap_err()
            .contains("invalid User-ID"));
        let err = configured_user_id_with(None, Some("x&y"), None).unwrap_err();
        assert!(
            err.contains("LEAD_TUTOR_USER_ID") && err.contains("invalid User-ID"),
            "{err}"
        );
        let missing = dir.path().join("lead_tutor.json");
        assert_eq!(read_saved_user_id(&missing).unwrap(), None);
        for content in [
            "{",
            "[]",
            "{\"other\": 1}",
            "{\"user_id\": 5}",
            "{\"user_id\": \"a b\"}",
        ] {
            let p = saved_file(dir.path(), content);
            let err = read_saved_user_id(&p).unwrap_err();
            assert!(err.contains(&p.display().to_string()), "{content}: {err}");
            assert!(err.contains("dbsloc tutor user-id --set"), "{err}");
            assert!(configured_user_id_with(None, None, Some(&p)).is_err());
        }
    }

    #[test]
    fn a_saved_id_round_trips_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested/deeper/lead_tutor.json");
        save_user_id(&p, " run-7 ").unwrap();
        assert_eq!(read_saved_user_id(&p).unwrap().as_deref(), Some("run-7"));
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.ends_with("}\n"), "{text:?}");
        save_user_id(&p, "run-8").unwrap();
        assert_eq!(read_saved_user_id(&p).unwrap().as_deref(), Some("run-8"));
        assert!(save_user_id(&p, "a b").is_err());
        let leftovers: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    }

    #[test]
    fn generated_ids_have_the_documented_shape_and_differ() {
        let (a, b) = (generate_user_id(), generate_user_id());
        for id in [&a, &b] {
            let hex = id.strip_prefix("dbsloc-").unwrap();
            assert_eq!(hex.len(), 12, "{id}");
            assert!(
                hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "{id}"
            );
            assert!(check_user_id(id).is_ok());
        }
        assert_ne!(a, b);
    }

    fn read_request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..pos]).into_owned();
                let len = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let body_start = pos + 4;
                while buf.len() < body_start + len {
                    let n = stream.read(&mut chunk).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                return (head, buf[body_start..body_start + len].to_vec());
            }
        }
        (String::new(), Vec::new())
    }

    fn serve_once(response: &'static str) -> (String, std::thread::JoinHandle<(String, Vec<u8>)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/my-endpoint", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            request
        });
        (url, handle)
    }

    #[test]
    fn post_sends_method_path_headers_and_exact_body() {
        let (url, handle) =
            serve_once("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let body = body_from_value(&json!({"pt_15454": patient()})).unwrap();
        let reply = post_payload(&url, "482197", &body).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.text, "ok");
        assert!(reply.is_success());
        let (head, got_body) = handle.join().unwrap();
        let mut lines = head.lines();
        assert_eq!(lines.next().unwrap(), "POST /my-endpoint HTTP/1.1");
        let header = |name: &str| {
            head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
            })
        };
        assert_eq!(header("content-type").as_deref(), Some("application/json"));
        assert_eq!(header("user-id").as_deref(), Some("482197"));
        assert_eq!(got_body, body);
    }

    #[test]
    fn an_error_status_is_returned_with_its_body() {
        let (url, handle) = serve_once(
            "HTTP/1.1 422 Unprocessable Entity\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad",
        );
        let reply = post_payload(&url, "u", b"{}").unwrap();
        assert_eq!(reply.status, 422);
        assert_eq!(reply.reason, "Unprocessable Entity");
        assert_eq!(reply.text, "bad");
        assert!(!reply.is_success());
        handle.join().unwrap();
    }

    #[test]
    fn a_refused_connection_is_a_network_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", listener.local_addr().unwrap());
        drop(listener);
        assert!(post_payload(&url, "u", b"{}").is_err());
    }

    #[test]
    #[ignore = "needs DBSLOC_TUTOR_PARITY_ROOT (dataset root) and DBSLOC_TUTOR_PARITY_OUT"]
    fn dump_real_payloads() {
        let (Ok(root), Ok(out)) = (
            std::env::var("DBSLOC_TUTOR_PARITY_ROOT"),
            std::env::var("DBSLOC_TUTOR_PARITY_OUT"),
        ) else {
            eprintln!("DBSLOC_TUTOR_PARITY_ROOT / DBSLOC_TUTOR_PARITY_OUT not set: skipping");
            return;
        };
        let (root, out) = (PathBuf::from(root), PathBuf::from(out));
        std::fs::create_dir_all(&out).unwrap();
        for subj in LEAD_TUTOR_PATIENT_KEYS {
            let id = &subj[3..];
            let rpath = root.join("derivatives/leaddbs").join(format!(
                "sub-{id}/reconstruction/sub-{id}_desc-reconstruction.json"
            ));
            let payload = payload_from_recon_json(&rpath).unwrap();
            std::fs::write(
                out.join(format!("{id}.json")),
                serde_json::to_vec(&payload).unwrap(),
            )
            .unwrap();
        }
        let merged = merge_payloads(&root, true).unwrap();
        std::fs::write(
            out.join("merged.json"),
            serde_json::to_vec(&merged.payload).unwrap(),
        )
        .unwrap();
    }
}
