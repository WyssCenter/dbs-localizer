// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! BIDS subject path resolution (`specs/01_bids.md` §6, §9–§11).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::PipelineError;

static ANCHOR_PRIORITY: &[&str] = &["T1w", "T2w", "PDw", "T2starw", "FLAIR", "FGATIR"];

pub const E_NO_SUBJ: &str = "E_NO_SUBJ";
pub const E_NO_ANCHOR: &str = "E_NO_ANCHOR";
pub const E_BIDS_PARSE: &str = "E_BIDS_PARSE";
pub const E_DUPL_RAW: &str = "E_DUPL_RAW";
/// The requested post-op modality has no image (`specs/01` §11).
pub const E_MISS_POSTOP: &str = "E_MISS_POSTOP";

/// Which post-op images the pipeline uses when a subject has both a CT and an MR.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum PostopModality {
    /// CT when a CT exists, else MRI; a stored per-subject choice in `uiprefs.mat` wins.
    #[default]
    Auto,
    Ct,
    Mri,
}

impl PostopModality {
    pub const CHOICES: [&'static str; 3] = ["auto", "ct", "mri"];

    pub fn parse(raw: &str) -> Result<PostopModality, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(PostopModality::Auto),
            "ct" => Ok(PostopModality::Ct),
            "mri" | "mr" => Ok(PostopModality::Mri),
            other => Err(format!(
                "unknown post-op modality {other:?} (choose one of {})",
                Self::CHOICES.join(", ")
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PostopModality::Auto => "auto",
            PostopModality::Ct => "ct",
            PostopModality::Mri => "mri",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BidsError {
    pub code: &'static str,
    pub message: String,
}

impl BidsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        BidsError {
            code,
            message: message.into(),
        }
    }
}

impl From<BidsError> for PipelineError {
    fn from(e: BidsError) -> Self {
        PipelineError::Bids(e.message)
    }
}

/// Resolved per-subject paths.
#[derive(Debug, Clone)]
pub struct Subject {
    pub subj_id: String,
    /// `<output root>/sub-<id>`; the output root defaults to `derivatives/leaddbs`
    /// ([`crate::paths::derivatives_root`]).
    pub subj_dir: PathBuf,
    /// `rawdata/sub-<id>`
    pub rawdata_dir: PathBuf,
    pub sourcedata_dir: PathBuf,
    /// `derivatives/leaddbs/sub-<id>/preprocessing/anat`
    pub preproc_dir: PathBuf,
    /// `derivatives/leaddbs/sub-<id>/coregistration/anat`
    pub coreg_dir: PathBuf,
    /// `derivatives/leaddbs/sub-<id>/normalization/anat`
    pub norm_dir: PathBuf,
    /// `derivatives/leaddbs/sub-<id>/reconstruction`
    pub recon_dir: PathBuf,
    pub prefs_dir: PathBuf,
    pub log_dir: PathBuf,
    pub brainshift_dir: PathBuf,
    pub stim_dir: PathBuf,

    /// BIDS suffix of pre-op anchor (e.g. `"T1w"`).
    pub anchor_modality: String,
    pub postop_modality: Option<String>,
    pub both_mrct_present: bool,
    pub preop_raw: OrderedPaths,
    pub postop_raw: OrderedPaths,
    /// Raw pre-op anchor NIfTI (rawdata).
    pub preop_anchor_raw: Option<PathBuf>,
    /// N4-corrected pre-op anchor (preproc_dir); set when the file already exists.
    pub preop_anchor_preproc: Option<PathBuf>,

    /// Raw post-op CT (rawdata ses-postop/anat).
    pub postop_ct_raw: Option<PathBuf>,
    /// Coregistration bundle (`specs/01` §6.4, §7a), filled for files that exist.
    pub coreg: CoregBundle,
    /// Normalization bundle (`specs/01` §6.5, §7a), filled for files that exist.
    pub norm: NormBundle,
}

#[derive(Debug, Clone, Default)]
pub struct CoregBundle {
    pub anat_preop: OrderedPaths,
    pub anat_postop: OrderedPaths,
    pub transform: OrderedPaths,
    pub log_method: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct NormBundle {
    pub template_space: Option<String>,
    pub anat_preop: OrderedPaths,
    pub anat_postop: OrderedPaths,
    pub transform_forward: Option<PathBuf>,
    pub transform_inverse: Option<PathBuf>,
    pub log_method: Option<PathBuf>,
}

impl Subject {
    /// Best available pre-op anchor: preproc if ready, else raw.
    pub fn anchor_best(&self) -> Option<&Path> {
        self.preop_anchor_preproc
            .as_deref()
            .or(self.preop_anchor_raw.as_deref())
    }

    pub fn anchor_in_use(&self) -> Option<PathBuf> {
        let precoreg = crate::derivatives::precoreg_image(self);
        if precoreg.is_file() {
            return Some(precoreg);
        }
        self.anchor_best().map(Path::to_path_buf)
    }

    pub fn postop_mr_images(&self) -> Vec<(&'static str, &Path)> {
        POSTOP_MR_KEYS
            .iter()
            .filter_map(|key| {
                self.postop_raw
                    .get(key)
                    .map(PathBuf::as_path)
                    .filter(|p| p.is_file())
                    .map(|p| (*key, p))
            })
            .collect()
    }

    pub fn norm_postop_mr_in_use(&self) -> Option<PathBuf> {
        POSTOP_MR_KEYS
            .iter()
            .find_map(|key| self.norm.anat_postop.get(key).cloned())
    }

    pub fn norm_postop_images(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.norm.anat_postop.values().cloned().collect();
        sort_paths(&mut out);
        out.dedup();
        out
    }

    pub fn locks_dir(&self) -> PathBuf {
        self.subj_dir.join("locks")
    }

    pub fn uiprefs_path(&self) -> PathBuf {
        crate::derivatives::uiprefs(self)
    }

    pub fn reconstruction_json(&self) -> PathBuf {
        crate::derivatives::reconstruction_json(self)
    }

    pub fn preop_t2_nonanchor(&self) -> Option<&Path> {
        if self.anchor_modality == "T2w" {
            return None;
        }
        self.preop_raw.get("T2w").map(PathBuf::as_path)
    }

    pub fn has_postop_ct(&self) -> bool {
        self.postop_ct_raw.as_deref().is_some_and(Path::is_file)
    }

    pub fn to_json(&self) -> Value {
        let map = |m: &OrderedPaths| -> Value {
            Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), json!(v.to_string_lossy())))
                    .collect(),
            )
        };
        let opt = |p: &Option<PathBuf>| -> Value {
            p.as_ref()
                .map_or(Value::Null, |p| json!(p.to_string_lossy()))
        };
        json!({
            "subj_id": self.subj_id,
            "subj_dir": self.subj_dir.to_string_lossy(),
            "rawdata_dir": self.rawdata_dir.to_string_lossy(),
            "anchor_modality": self.anchor_modality,
            "postop_modality": self.postop_modality,
            "both_mrct_present": self.both_mrct_present,
            "preop_raw": map(&self.preop_raw),
            "postop_raw": map(&self.postop_raw),
            "preop_anchor_preproc": opt(&self.preop_anchor_preproc),
            "preproc_dir": self.preproc_dir.to_string_lossy(),
            "coreg_dir": self.coreg_dir.to_string_lossy(),
            "norm_dir": self.norm_dir.to_string_lossy(),
            "recon_dir": self.recon_dir.to_string_lossy(),
            "prefs_dir": self.prefs_dir.to_string_lossy(),
            "locks_dir": self.locks_dir().to_string_lossy(),
            "uiprefs": self.uiprefs_path().to_string_lossy(),
            "reconstruction_json": self.reconstruction_json().to_string_lossy(),
        })
    }
}

pub const POSTOP_MR_KEYS: [&str; 3] = ["ax_MRI", "cor_MRI", "sag_MRI"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderedPaths(Vec<(String, PathBuf)>);

impl OrderedPaths {
    pub fn get(&self, key: &str) -> Option<&PathBuf> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn insert_if_absent(&mut self, key: String, value: PathBuf) {
        if !self.contains_key(&key) {
            self.0.push((key, value));
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PathBuf)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl Iterator<Item = &PathBuf> {
        self.0.iter().map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub fn path_sort_key(p: &Path) -> (String, String) {
    let s = p.to_string_lossy().into_owned();
    (s.to_lowercase(), s)
}

pub fn sort_paths(paths: &mut [PathBuf]) {
    paths.sort_by_cached_key(|p| path_sort_key(p));
}

/// Strip Windows' extended-length `\\?\` prefix from a canonicalized path.
///
/// `Path::canonicalize` returns the *verbatim* form on Windows, and every path derived from the
/// dataset root inherits it. That leaks `\\?\C:\...` into written JSON (the scene manifest's
/// `reco_json`) and into log lines, and — worse — gives the same file two different string
/// identities, so a path-keyed record written by one code path does not match the other.
///
/// * `\\?\C:\foo` becomes `C:\foo`
/// * `\\?\UNC\server\share` becomes `\\server\share`
/// * anything else is returned unchanged, so this is a no-op off Windows
///
/// Only a drive-letter path is shortened; other verbatim forms have no equivalent
/// short spelling.
pub fn simplify_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let mut ch = rest.chars();
        if matches!((ch.next(), ch.next(), ch.next()), (Some(c), Some(':'), Some('\\')) if c.is_ascii_alphabetic())
        {
            return PathBuf::from(rest);
        }
    }
    p
}

pub fn parse_subj_id(folder_name: &str) -> Result<String, BidsError> {
    let id = folder_name
        .strip_prefix("sub-")
        .filter(|rest| !rest.is_empty() && rest.chars().all(char::is_alphanumeric));
    id.map(str::to_owned).ok_or_else(|| {
        BidsError::new(
            E_BIDS_PARSE,
            format!("not a valid sub-<id> folder name: {folder_name:?}"),
        )
    })
}

fn folder_sub(subj_id: &str) -> String {
    let s = subj_id.trim();
    if s.starts_with("sub-") {
        s.to_owned()
    } else {
        format!("sub-{s}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawImageEntry {
    pub path: String,
    pub session: String,
    pub datatype: String,
    pub entities: serde_json::Map<String, Value>,
}

fn index_err(path: &Path, what: impl std::fmt::Display) -> BidsError {
    BidsError::new(E_BIDS_PARSE, format!("{}: {what}", path.display()))
}

pub fn load_rawimages(path: &Path) -> Result<Vec<RawImageEntry>, BidsError> {
    let text = std::fs::read_to_string(path).map_err(|e| index_err(path, e))?;
    let data: Value = serde_json::from_str(&text).map_err(|e| index_err(path, e))?;
    let rows = data
        .as_array()
        .ok_or_else(|| index_err(path, "rawimages root must be a list"))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let obj = row
            .as_object()
            .ok_or_else(|| index_err(path, "rawimages row must be an object"))?;
        let s = |k: &str| {
            obj.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        out.push(RawImageEntry {
            path: s("path"),
            session: s("session"),
            datatype: s("datatype"),
            entities: obj
                .get("entities")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

pub fn logical_key_to_bids_suffix(key: &str) -> String {
    let k = key.trim();
    if k == "CT" || k.ends_with("_CT") {
        return "CT".to_owned();
    }
    for suf in [
        "T1w", "T2w", "T2starw", "PDw", "FLAIR", "FGATIR", "T1map", "T2map",
    ] {
        if k.ends_with(suf) || k.contains(&format!("_{suf}")) || k == suf {
            return suf.to_owned();
        }
    }
    if k.contains("T1") {
        return "T1w".to_owned();
    }
    if k.contains("T2") {
        return "T2w".to_owned();
    }
    k.to_owned()
}

pub fn postop_bundle_key(logical: &str, filename: &str) -> String {
    let lo = logical.trim();
    if lo.eq_ignore_ascii_case("CT") {
        return "CT".to_owned();
    }
    let nl = filename.to_lowercase();
    if nl.ends_with("_ct.nii") || nl.ends_with("_ct.nii.gz") || nl.contains("_ct.nii") {
        return "CT".to_owned();
    }
    if nl.contains("acq-cor") || nl.contains("_cor_") {
        return "cor_MRI".to_owned();
    }
    if nl.contains("acq-sag") || nl.contains("_sag_") {
        return "sag_MRI".to_owned();
    }
    if nl.contains("acq-ax") {
        return "ax_MRI".to_owned();
    }
    if lo.eq_ignore_ascii_case("cor_MRI") {
        return "cor_MRI".to_owned();
    }
    if lo.eq_ignore_ascii_case("sag_MRI") {
        return "sag_MRI".to_owned();
    }
    "ax_MRI".to_owned()
}

fn nifti_in_session(
    root: &Path,
    subj: &str,
    session: &str,
    stem: &str,
    planned: &[PathBuf],
) -> Option<PathBuf> {
    let anat = root
        .join("rawdata")
        .join(format!("sub-{subj}"))
        .join(format!("ses-{session}"))
        .join("anat");
    let planned_here: Vec<PathBuf> = planned
        .iter()
        .filter(|p| p.parent() == Some(anat.as_path()))
        .cloned()
        .collect();
    if !anat.is_dir() && planned_here.is_empty() {
        return None;
    }
    let present = |p: &Path| p.is_file() || planned_here.iter().any(|q| q == p);
    for ext in [".nii.gz", ".nii"] {
        let c = anat.join(format!("{stem}{ext}"));
        if present(&c) {
            return Some(c);
        }
    }
    let mut names: Vec<PathBuf> = std::fs::read_dir(&anat)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .chain(planned_here.iter().cloned())
        .collect();
    names.sort_by_cached_key(|p| {
        let n = file_name(p);
        (n.to_lowercase(), n)
    });
    names.into_iter().find(|p| {
        let n = file_name(p);
        present(p) && n.starts_with(stem) && is_nifti(&n)
    })
}

pub fn load_leaddbs_prefs_index(
    path: &Path,
    root: &Path,
    subj: &str,
) -> Result<Vec<RawImageEntry>, BidsError> {
    load_leaddbs_prefs_index_with(path, root, subj, &[])
}

pub fn load_leaddbs_prefs_index_with(
    path: &Path,
    root: &Path,
    subj: &str,
    planned: &[PathBuf],
) -> Result<Vec<RawImageEntry>, BidsError> {
    let text = std::fs::read_to_string(path).map_err(|e| index_err(path, e))?;
    let data: Value = serde_json::from_str(&text).map_err(|e| index_err(path, e))?;
    let top = data
        .as_object()
        .ok_or_else(|| index_err(path, "Lead-DBS rawimages top level must be an object"))?;
    let mut out = Vec::new();
    for session in ["preop", "postop"] {
        let Some(anat) = top
            .get(session)
            .and_then(Value::as_object)
            .and_then(|s| s.get("anat"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (logical, stem) in anat {
            let Some(stem) = stem.as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let Some(p) = nifti_in_session(root, subj, session, stem, planned) else {
                continue;
            };
            let mut entities = serde_json::Map::new();
            entities.insert("suffix".into(), json!(logical_key_to_bids_suffix(logical)));
            entities.insert("modality".into(), json!(logical));
            out.push(RawImageEntry {
                path: p.to_string_lossy().into_owned(),
                session: session.to_owned(),
                datatype: "anat".to_owned(),
                entities,
            });
        }
    }
    Ok(out)
}

pub fn prefs_rawimages_path(subj_dir_leaddbs: &Path, sid: &str) -> PathBuf {
    crate::derivatives::prefs_rawimages(subj_dir_leaddbs, sid)
}

fn sorted_json(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", json!(k), sorted_json(&m[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(sorted_json).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

pub fn check_duplicates(entries: &[RawImageEntry], subj: &str) -> Result<(), BidsError> {
    let mut counts: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for e in entries {
        let key = (
            e.session.clone(),
            e.datatype.clone(),
            sorted_json(&Value::Object(e.entities.clone())),
        );
        *counts.entry(key).or_default() += 1;
    }
    let dups: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|((s, d, ent), _)| format!("({s:?}, {d:?}, {ent})"))
        .collect();
    if dups.is_empty() {
        Ok(())
    } else {
        Err(BidsError::new(
            E_DUPL_RAW,
            format!(
                "duplicate index rows for sub-{subj}: [{}] (E_DUPL_RAW)",
                dups.join(", ")
            ),
        ))
    }
}

fn collect_index_entries(
    root: &Path,
    raw: &Path,
    dlb: &Path,
    sid: &str,
) -> Result<Vec<RawImageEntry>, BidsError> {
    let mut out = Vec::new();
    let rp = raw.join("rawimages.json");
    if rp.is_file() {
        out.extend(load_rawimages(&rp)?);
    }
    let pr = prefs_rawimages_path(dlb, sid);
    if pr.is_file() {
        out.extend(load_leaddbs_prefs_index(&pr, root, sid)?);
    }
    check_duplicates(&out, sid)?;
    Ok(out)
}

fn discover_preop_from_anat(anat: &Path) -> OrderedPaths {
    let mut files = walkdir_niftis(anat);
    sort_paths(&mut files);
    let mut out = OrderedPaths::default();
    for p in files {
        let name = file_name(&p);
        for suf in ANCHOR_PRIORITY {
            if name.ends_with(&format!("_{suf}.nii")) || name.ends_with(&format!("_{suf}.nii.gz")) {
                out.insert_if_absent((*suf).to_owned(), p.clone());
                break;
            }
        }
    }
    out
}

fn discover_postop_from_anat(anat: &Path, indexed: &HashSet<PathBuf>) -> OrderedPaths {
    let mut files = walkdir_niftis(anat);
    sort_paths(&mut files);
    let mut out = OrderedPaths::default();
    for p in files.into_iter().filter(|p| !indexed.contains(p)) {
        let n = file_name(&p).to_lowercase();
        let k = if n.ends_with("_ct.nii") || n.ends_with("_ct.nii.gz") {
            "CT"
        } else if n.contains("acq-cor") {
            "cor_MRI"
        } else if n.contains("acq-sag") {
            "sag_MRI"
        } else {
            "ax_MRI"
        };
        out.insert_if_absent(k.to_owned(), p);
    }
    out
}

fn select_anchor_modality(mods: &OrderedPaths) -> String {
    for pref in ANCHOR_PRIORITY {
        if mods.contains_key(pref) {
            return (*pref).to_owned();
        }
    }
    let mut keys: Vec<&String> = mods.keys().collect();
    keys.sort_by_key(|k| k.to_lowercase());
    keys.first().map(|s| (*s).clone()).unwrap_or_default()
}

fn postop_has_mr_file(postop_anat: &Path) -> bool {
    postop_anat.is_dir()
        && walkdir_niftis(postop_anat).iter().any(|p| {
            let n = file_name(p).to_lowercase();
            ["acq-ax", "acq-cor", "acq-sag"]
                .iter()
                .any(|acq| n.contains(acq))
        })
}

fn choose_postop_modality(
    has_ct: bool,
    has_mr: bool,
    choice: PostopModality,
    stored: impl FnOnce() -> Option<PostopModality>,
) -> Result<Option<String>, PostopModality> {
    let ct = || Some("CT".to_owned());
    let mri = || Some("MRI".to_owned());
    match choice {
        PostopModality::Ct => has_ct.then(ct).ok_or(PostopModality::Ct),
        PostopModality::Mri => has_mr.then(mri).ok_or(PostopModality::Mri),
        PostopModality::Auto => Ok(match (has_ct, has_mr) {
            (false, false) => None,
            (true, false) => ct(),
            (false, true) => mri(),
            (true, true) => match stored() {
                Some(PostopModality::Mri) => mri(),
                _ => ct(),
            },
        }),
    }
}

/// [`resolve_subject_in`] with the default output root `<dataset_root>/derivatives/leaddbs`.
pub fn resolve_subject_with(
    dataset_root: &Path,
    subj_id: &str,
    use_index: bool,
) -> Result<Subject, BidsError> {
    resolve_subject_in(dataset_root, None, subj_id, use_index)
}

/// Resolve a subject whose raw images live under `dataset_root/rawdata` and whose pipeline
/// outputs live under [`crate::paths::derivatives_root`]`(dataset_root, output_dir)`.
pub fn resolve_subject_in(
    dataset_root: &Path,
    output_dir: Option<&Path>,
    subj_id: &str,
    use_index: bool,
) -> Result<Subject, BidsError> {
    resolve_subject_for(
        dataset_root,
        output_dir,
        subj_id,
        use_index,
        PostopModality::Auto,
    )
}

/// [`resolve_subject_in`] with an explicit post-op modality choice (`specs/01` §10 step 5).
/// `Ct` / `Mri` fail with `E_MISS_POSTOP` when the subject has no such post-op image.
pub fn resolve_subject_for(
    dataset_root: &Path,
    output_dir: Option<&Path>,
    subj_id: &str,
    use_index: bool,
    postop_choice: PostopModality,
) -> Result<Subject, BidsError> {
    let sid = parse_subj_id(&folder_sub(subj_id))?;
    let root = crate::paths::absolute_dataset_root(dataset_root);
    let raw = root.join("rawdata").join(format!("sub-{sid}"));
    let src = root.join("sourcedata").join(format!("sub-{sid}"));
    let deriv_root = crate::paths::derivatives_root(&root, output_dir);
    crate::outputs::register_derivatives_root(&deriv_root);
    let dlb = deriv_root.join(format!("sub-{sid}"));
    if !raw.is_dir() {
        return Err(BidsError::new(
            E_NO_SUBJ,
            format!(
                "expected {} (BIDS: rawdata/sub-<id>/). Create it under the dataset root {} or \
                 pass the correct --dataset-root. (E_NO_SUBJ)",
                raw.display(),
                root.display()
            ),
        ));
    }

    let preop_anat_dir = raw.join("ses-preop").join("anat");
    let postop_anat_dir = raw.join("ses-postop").join("anat");
    let mut modalities = OrderedPaths::default();
    let mut postop = OrderedPaths::default();
    let mut indexed_postop: HashSet<String> = HashSet::new();
    if use_index {
        for e in collect_index_entries(&root, &raw, &dlb, &sid)? {
            if e.datatype != "anat" || e.path.is_empty() {
                continue;
            }
            let p = crate::paths::absolute(&root.join(&e.path));
            if !p.is_file() {
                continue;
            }
            if e.session == "preop" {
                if let Some(suf) = e.entities.get("suffix").and_then(Value::as_str) {
                    modalities.insert_if_absent(suf.to_owned(), p);
                }
            } else if e.session == "postop" {
                let entity = |name: &str| {
                    e.entities
                        .get(name)
                        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                };
                let modality = e
                    .entities
                    .get("modality")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let logical = if !modality.trim().is_empty() {
                    modality.to_owned()
                } else if entity("suffix").is_some_and(|v| v.trim().eq_ignore_ascii_case("CT")) {
                    "CT".to_owned()
                } else {
                    match entity("acq").map(|v| v.trim().to_ascii_lowercase()) {
                        Some(a) if matches!(a.as_str(), "ax" | "cor" | "sag") => {
                            format!("{a}_MRI")
                        }
                        _ => String::new(),
                    }
                };
                let key = postop_bundle_key(&logical, &file_name(&p));
                if !postop.contains_key(&key) {
                    indexed_postop.insert(key.clone());
                }
                postop.insert_if_absent(key, p);
            }
        }
    }
    if modalities.is_empty() && preop_anat_dir.is_dir() {
        modalities = discover_preop_from_anat(&preop_anat_dir);
    }
    let anchor = select_anchor_modality(&modalities);
    if anchor.is_empty() {
        return Err(BidsError::new(
            E_NO_ANCHOR,
            format!(
                "no pre-op anchor: add a NIfTI under {} (e.g. *T1w.nii.gz) or list files in {} / \
                 prefs index. (E_NO_ANCHOR)",
                preop_anat_dir.display(),
                raw.join("rawimages.json").display()
            ),
        ));
    }
    let indexed_paths: HashSet<PathBuf> = postop.values().cloned().collect();
    for (k, p) in discover_postop_from_anat(&postop_anat_dir, &indexed_paths).iter() {
        postop.insert_if_absent(k.clone(), p.clone());
    }
    let has_ct = postop.contains_key("CT");
    let has_mr = postop_has_mr_file(&postop_anat_dir)
        || POSTOP_MR_KEYS.iter().any(|k| indexed_postop.contains(*k));

    let preproc_dir = dlb.join("preprocessing").join("anat");
    let coreg_dir = dlb.join("coregistration").join("anat");
    let norm_dir = dlb.join("normalization").join("anat");
    let anchor_raw = modalities.get(&anchor).cloned();

    let mut sub = Subject {
        subj_id: sid.clone(),
        subj_dir: dlb.clone(),
        rawdata_dir: raw,
        sourcedata_dir: src,
        preproc_dir,
        coreg_dir,
        norm_dir,
        recon_dir: dlb.join("reconstruction"),
        prefs_dir: dlb.join("prefs"),
        log_dir: dlb.join("log"),
        brainshift_dir: dlb.join("brainshift").join("anat"),
        stim_dir: dlb.join("stimulations"),
        anchor_modality: anchor,
        postop_modality: None,
        both_mrct_present: has_ct && has_mr,
        postop_ct_raw: postop.get("CT").cloned(),
        preop_raw: modalities,
        postop_raw: postop,
        preop_anchor_raw: anchor_raw,
        preop_anchor_preproc: None,
        coreg: CoregBundle::default(),
        norm: NormBundle::default(),
    };
    let uiprefs = sub.uiprefs_path();
    sub.postop_modality = choose_postop_modality(has_ct, has_mr, postop_choice, || {
        uiprefs
            .is_file()
            .then(|| crate::uiprefs::postop_modality_from_uiprefs(&uiprefs).ok().flatten())
            .flatten()
    })
    .map_err(|wanted| {
        BidsError::new(
            E_MISS_POSTOP,
            format!(
                "post-op modality {} was requested but sub-{sid} has no post-op {} image                  (found: {}). (E_MISS_POSTOP)",
                wanted.as_str(),
                if wanted == PostopModality::Ct { "CT" } else { "MR" },
                match (has_ct, has_mr) {
                    (true, _) => "CT",
                    (false, true) => "MR",
                    (false, false) => "none",
                }
            ),
        )
    })?;
    sub.refresh_bundles();
    Ok(sub)
}

impl Subject {
    pub fn refresh_bundles(&mut self) {
        use crate::derivatives as d;
        let existing = |p: PathBuf| p.is_file().then_some(p);

        self.preop_anchor_preproc = existing(d::preproc_image(self));

        let mut coreg = CoregBundle::default();
        if let Some(p) = existing(d::precoreg_image(self)) {
            coreg
                .anat_preop
                .insert_if_absent(self.anchor_modality.clone(), p);
        }
        if self.preop_t2_nonanchor().is_some() {
            if let Some(p) = existing(d::coreg_t2_image(self)) {
                coreg.anat_preop.insert_if_absent("T2w".to_owned(), p);
            }
            if let Some(p) = existing(d::coreg_t2_affine(self)) {
                coreg.transform.insert_if_absent("T2w".to_owned(), p);
            }
        }
        if self.postop_ct_raw.is_some() {
            if let Some(p) = existing(d::coreg_ct_image(self)) {
                coreg.anat_postop.insert_if_absent("CT".to_owned(), p);
            }
            if let Some(p) = existing(d::coreg_ct_affine(self)) {
                coreg.transform.insert_if_absent("CT".to_owned(), p);
            }
        }
        for key in POSTOP_MR_KEYS {
            if !self.postop_raw.contains_key(key) {
                continue;
            }
            if let Some(p) = existing(d::coreg_mr_image(self, key)) {
                coreg.anat_postop.insert_if_absent(key.to_owned(), p);
            }
            if let Some(p) = existing(d::coreg_mr_affine(self, key)) {
                coreg.transform.insert_if_absent(key.to_owned(), p);
            }
        }
        coreg.log_method = existing(d::coreg_method_log(self));
        self.coreg = coreg;

        let mut norm = NormBundle {
            log_method: existing(d::norm_method_log(self)),
            ..NormBundle::default()
        };
        let space = norm
            .log_method
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| {
                v.get("template_space")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        norm.template_space = space.clone();
        if let Some(space) = space {
            if let Some(p) = existing(d::norm_anchor_image(self, &space)) {
                norm.anat_preop
                    .insert_if_absent(self.anchor_modality.clone(), p);
            }
            if self.postop_ct_raw.is_some() {
                if let Some(p) = existing(d::norm_postop_image(self, "CT", &space)) {
                    norm.anat_postop.insert_if_absent("CT".to_owned(), p);
                }
            }
            for key in POSTOP_MR_KEYS {
                if !self.postop_raw.contains_key(key) {
                    continue;
                }
                if let Some(p) = existing(d::norm_postop_image(self, key, &space)) {
                    norm.anat_postop.insert_if_absent(key.to_owned(), p);
                }
            }
            norm.transform_forward = existing(d::norm_forward(self, &space));
            norm.transform_inverse = existing(d::norm_inverse(self, &space));
        }
        self.norm = norm;
    }
}

/// List unique subject IDs (without `sub-` prefix) under `dataset_root` and its default output
/// root `derivatives/leaddbs`.
pub fn list_subject_ids(dataset_root: &Path) -> Vec<String> {
    list_subject_ids_in(dataset_root, None)
}

/// List unique subject IDs (without `sub-` prefix) in the raw folders of `dataset_root` and in
/// the output root [`crate::paths::derivatives_root`]`(dataset_root, output_dir)`.
pub fn list_subject_ids_in(dataset_root: &Path, output_dir: Option<&Path>) -> Vec<String> {
    let mut ids: HashSet<String> = HashSet::new();
    let mut dirs: Vec<PathBuf> = ["rawdata", "sourcedata", "derivatives"]
        .iter()
        .map(|d| dataset_root.join(d))
        .collect();
    dirs.push(crate::paths::derivatives_root(dataset_root, output_dir));
    for p in dirs {
        if let Ok(entries) = std::fs::read_dir(&p) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("sub-") && name.len() > 4 && e.path().is_dir() {
                    ids.insert(name[4..].to_owned());
                }
            }
        }
    }
    let mut v: Vec<String> = ids.into_iter().collect();
    v.sort();
    v
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn is_nifti(name: &str) -> bool {
    name.ends_with(".nii") || name.ends_with(".nii.gz")
}

pub fn walkdir_niftis(dir: &Path) -> Vec<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    let mut results = Vec::new();
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                let Ok(ft) = e.file_type() else {
                    continue;
                };
                if ft.is_dir() {
                    stack.push(p);
                } else if !ft.is_dir()
                    && p.is_file()
                    && is_nifti(&name)
                    && !crate::outputs::is_partial_name(&name)
                {
                    results.push(p);
                }
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_ids_parse_like_bids() {
        assert_eq!(parse_subj_id("sub-01").unwrap(), "01");
        assert_eq!(parse_subj_id("sub-abc123").unwrap(), "abc123");
        for bad in ["sub-", "sub-a_b", "sub-a-b", "01", "sub-a b"] {
            assert_eq!(parse_subj_id(bad).unwrap_err().code, E_BIDS_PARSE, "{bad}");
        }
        assert_eq!(folder_sub("42"), "sub-42");
        assert_eq!(folder_sub("sub-42"), "sub-42");
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_bids_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    #[test]
    fn resolves_from_anat_without_index() {
        let root = scratch("anat");
        let anat = root.join("rawdata/sub-001/ses-preop/anat");
        touch(&anat.join("sub-001_ses-preop_T2w.nii.gz"));
        touch(&anat.join("sub-001_ses-preop_acq-b_T1w.nii.gz"));
        touch(&anat.join("sub-001_ses-preop_acq-a_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "001", true).unwrap();
        assert_eq!(sub.subj_id, "001");
        assert_eq!(sub.anchor_modality, "T1w");
        assert!(sub
            .preop_anchor_raw
            .as_ref()
            .unwrap()
            .ends_with("sub-001_ses-preop_acq-a_T1w.nii.gz"));
        assert_eq!(sub.preop_raw.len(), 2);
        assert_eq!(sub.postop_modality, None);
        assert!(sub
            .preop_t2_nonanchor()
            .unwrap()
            .ends_with("sub-001_ses-preop_T2w.nii.gz"));
        assert!(crate::derivatives::coreg_t2_affine(&sub).ends_with(
            "coregistration/transformations/sub-001_ses-preop_from-T2w_to-anchorNative_mode-image_xfm.mat"
        ));
        assert!(crate::derivatives::norm_method_log(&sub)
            .ends_with("normalization/log/sub-001_desc-normmethod.json"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn t2_anchor_has_no_second_t2_channel() {
        let root = scratch("t2anchor");
        let anat = root.join("rawdata/sub-8/ses-preop/anat");
        touch(&anat.join("sub-8_ses-preop_acq-b_T2w.nii.gz"));
        touch(&anat.join("sub-8_ses-preop_acq-A_T2w.nii.gz"));
        let sub = resolve_subject_with(&root, "8", false).unwrap();
        assert_eq!(sub.anchor_modality, "T2w");
        assert!(sub.preop_t2_nonanchor().is_none());
        touch(&anat.join("sub-8_ses-preop_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "8", false).unwrap();
        assert!(sub
            .preop_t2_nonanchor()
            .unwrap()
            .ends_with("sub-8_ses-preop_acq-A_T2w.nii.gz"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_subject_and_anchor_have_codes() {
        let root = scratch("codes");
        let e = resolve_subject_with(&root, "1", true).unwrap_err();
        assert_eq!(e.code, E_NO_SUBJ);
        assert!(e.message.contains("rawdata/sub-<id>"));
        std::fs::create_dir_all(root.join("rawdata/sub-x/ses-preop/anat")).unwrap();
        let e = resolve_subject_with(&root, "x", true).unwrap_err();
        assert_eq!(e.code, E_NO_ANCHOR);
        assert!(e.message.contains("no pre-op anchor"));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_prefs(root: &Path, sid: &str, body: Value) {
        let p = root
            .join("derivatives/leaddbs")
            .join(format!("sub-{sid}"))
            .join("prefs")
            .join(format!("sub-{sid}_desc-rawimages.json"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body.to_string()).unwrap();
    }

    #[test]
    fn prefs_index_finds_postop_ct_and_hydrates_norm() {
        let root = scratch("prefs");
        touch(&root.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii"));
        touch(&root.join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_CT.nii"));
        write_prefs(
            &root,
            "1",
            json!({"preop": {"anat": {"T1w": "sub-1_ses-preop_T1w"}},
                   "postop": {"anat": {"CT": "sub-1_ses-postop_CT"}}}),
        );
        let deriv = root.join("derivatives/leaddbs/sub-1");
        touch(&deriv.join(
            "normalization/anat/sub-1_ses-postop_space-MNI152NLin2009bAsym_desc-preproc_CT.nii.gz",
        ));
        std::fs::create_dir_all(deriv.join("normalization/log")).unwrap();
        std::fs::write(
            deriv.join("normalization/log/sub-1_desc-normmethod.json"),
            r#"{"template_space": "MNI152NLin2009bAsym"}"#,
        )
        .unwrap();
        let sub = resolve_subject_with(&root, "1", true).unwrap();
        assert!(sub.postop_raw.contains_key("CT"));
        assert!(sub.postop_ct_raw.is_some());
        assert_eq!(sub.postop_modality.as_deref(), Some("CT"));
        assert!(sub
            .norm
            .anat_postop
            .get("CT")
            .unwrap()
            .ends_with("sub-1_ses-postop_space-MNI152NLin2009bAsym_desc-preproc_CT.nii.gz"));
        assert!(sub.norm.log_method.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prefs_logical_iso_key_maps_to_suffix() {
        let root = scratch("iso");
        touch(&root.join("rawdata/sub-9/ses-preop/anat/sub-9_ses-preop_acq-iso_T1w.nii"));
        write_prefs(
            &root,
            "9",
            json!({"preop": {"anat": {"iso_T1w": "sub-9_ses-preop_acq-iso_T1w"}}}),
        );
        let sub = resolve_subject_with(&root, "9", true).unwrap();
        assert!(sub.preop_raw.contains_key("T1w"));
        assert_eq!(logical_key_to_bids_suffix("ax_T2w"), "T2w");
        assert_eq!(logical_key_to_bids_suffix("CT"), "CT");
        assert_eq!(
            postop_bundle_key("x", "sub_ses_postop_acq-ax_T1w.nii.gz"),
            "ax_MRI"
        );
        assert_eq!(postop_bundle_key("CT", "anything.nii"), "CT");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_index_rows_are_refused() {
        let root = scratch("dupl");
        let t1 = root.join("rawdata/sub-2/ses-preop/anat/sub-2_ses-preop_T1w.nii");
        touch(&t1);
        let row = json!({"path": "rawdata/sub-2/ses-preop/anat/sub-2_ses-preop_T1w.nii",
                         "session": "preop", "datatype": "anat",
                         "entities": {"suffix": "T1w", "acq": "ax"}});
        let row2 = json!({"path": "rawdata/sub-2/ses-preop/anat/sub-2_ses-preop_T1w.nii",
                          "session": "preop", "datatype": "anat",
                          "entities": {"acq": "ax", "suffix": "T1w"}});
        std::fs::write(
            root.join("rawdata/sub-2/rawimages.json"),
            json!([row, row2]).to_string(),
        )
        .unwrap();
        let e = resolve_subject_with(&root, "2", true).unwrap_err();
        assert_eq!(e.code, E_DUPL_RAW);
        assert!(resolve_subject_with(&root, "2", false).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn store_uiprefs_modality(root: &Path, sid: &str, value: f64) {
        use fastwarp_matfile::mat5::{write_mat5_file, Mat5Value};
        let p = root.join(format!(
            "derivatives/leaddbs/sub-{sid}/prefs/sub-{sid}_desc-uiprefs.mat"
        ));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        write_mat5_file(
            &p,
            &[("modality".to_owned(), Mat5Value::scalar(value))],
            false,
        )
        .unwrap();
    }

    fn modality_for(root: &Path, sid: &str, choice: PostopModality) -> Result<String, BidsError> {
        resolve_subject_for(root, None, sid, false, choice)
            .map(|s| s.postop_modality.unwrap_or_else(|| "none".to_owned()))
    }

    #[test]
    fn ct_plus_mr_postop_picks_ct_unless_mri_is_chosen() {
        let root = scratch("ctmr");
        touch(&root.join("rawdata/sub-3/ses-preop/anat/sub-3_ses-preop_T1w.nii.gz"));
        let post = root.join("rawdata/sub-3/ses-postop/anat");
        touch(&post.join("sub-3_ses-postop_CT.nii.gz"));
        touch(&post.join("sub-3_ses-postop_acq-ax_T1w.nii.gz"));
        touch(&post.join("sub-3_ses-postop_acq-cor_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "3", false).unwrap();
        assert_eq!(sub.postop_modality.as_deref(), Some("CT"));
        assert!(sub.both_mrct_present);
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["ax_MRI", "cor_MRI", "CT"]);
        assert_eq!(
            modality_for(&root, "3", PostopModality::Mri).unwrap(),
            "MRI"
        );
        assert_eq!(modality_for(&root, "3", PostopModality::Ct).unwrap(), "CT");

        store_uiprefs_modality(&root, "3", 1.0);
        assert_eq!(
            modality_for(&root, "3", PostopModality::Auto).unwrap(),
            "MRI"
        );
        assert_eq!(modality_for(&root, "3", PostopModality::Ct).unwrap(), "CT");
        store_uiprefs_modality(&root, "3", 2.0);
        assert_eq!(
            modality_for(&root, "3", PostopModality::Auto).unwrap(),
            "CT"
        );
        store_uiprefs_modality(&root, "3", 3.0);
        assert_eq!(
            modality_for(&root, "3", PostopModality::Auto).unwrap(),
            "CT"
        );
        std::fs::write(
            root.join("derivatives/leaddbs/sub-3/prefs/sub-3_desc-uiprefs.mat"),
            b"garbage",
        )
        .unwrap();
        assert_eq!(
            modality_for(&root, "3", PostopModality::Auto).unwrap(),
            "CT"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_forced_post_op_modality_needs_its_image_and_a_stored_one_is_only_a_preference() {
        let root = scratch("forced");
        for sid in ["7", "8", "9"] {
            touch(&root.join(format!(
                "rawdata/sub-{sid}/ses-preop/anat/sub-{sid}_ses-preop_T1w.nii.gz"
            )));
        }
        touch(&root.join("rawdata/sub-7/ses-postop/anat/sub-7_ses-postop_CT.nii.gz"));
        touch(&root.join("rawdata/sub-8/ses-postop/anat/sub-8_ses-postop_acq-sag_T2w.nii.gz"));
        assert_eq!(
            modality_for(&root, "7", PostopModality::Auto).unwrap(),
            "CT"
        );
        assert_eq!(
            modality_for(&root, "8", PostopModality::Auto).unwrap(),
            "MRI"
        );
        assert_eq!(
            modality_for(&root, "9", PostopModality::Auto).unwrap(),
            "none"
        );
        for (sid, choice) in [
            ("7", PostopModality::Mri),
            ("8", PostopModality::Ct),
            ("9", PostopModality::Ct),
            ("9", PostopModality::Mri),
        ] {
            let e = modality_for(&root, sid, choice).unwrap_err();
            assert_eq!(e.code, E_MISS_POSTOP, "{sid} {choice:?}");
        }
        store_uiprefs_modality(&root, "7", 1.0);
        assert_eq!(
            modality_for(&root, "7", PostopModality::Auto).unwrap(),
            "CT"
        );
        store_uiprefs_modality(&root, "8", 2.0);
        assert_eq!(
            modality_for(&root, "8", PostopModality::Auto).unwrap(),
            "MRI"
        );
        assert_eq!(PostopModality::parse(" MR ").unwrap(), PostopModality::Mri);
        assert!(PostopModality::parse("pet").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_coronal_or_sagittal_post_op_mr_alone_makes_a_post_op_mri_subject() {
        for (n, acq, key) in [(1, "cor", "cor_MRI"), (2, "sag", "sag_MRI")] {
            let root = scratch(&format!("only_{acq}"));
            touch(&root.join(format!(
                "rawdata/sub-{n}/ses-preop/anat/sub-{n}_ses-preop_T1w.nii.gz"
            )));
            let post = root.join(format!("rawdata/sub-{n}/ses-postop/anat"));
            touch(&post.join(format!("sub-{n}_ses-postop_acq-{acq}_T2w.nii.gz")));
            let sub = resolve_subject_with(&root, &n.to_string(), false).unwrap();
            assert_eq!(sub.postop_modality.as_deref(), Some("MRI"), "{acq}");
            assert!(!sub.both_mrct_present);
            let keys: Vec<&str> = sub.postop_mr_images().iter().map(|(k, _)| *k).collect();
            assert_eq!(keys, [key]);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn an_index_row_naming_a_coronal_or_sagittal_mr_makes_a_post_op_mri_subject() {
        for (n, key) in [(1, "cor_MRI"), (2, "sag_MRI")] {
            let root = scratch(&format!("index_{key}"));
            touch(&root.join(format!(
                "rawdata/sub-{n}/ses-preop/anat/sub-{n}_ses-preop_T1w.nii.gz"
            )));
            let file = format!("sub-{n}_ses-postop_tse.nii.gz");
            touch(&root.join(format!("rawdata/sub-{n}/ses-postop/anat/{file}")));
            std::fs::write(
                root.join(format!("rawdata/sub-{n}/rawimages.json")),
                json!([{"path": format!("rawdata/sub-{n}/ses-postop/anat/{file}"),
                        "session": "postop", "datatype": "anat",
                        "entities": {"suffix": "T2w", "modality": key}}])
                .to_string(),
            )
            .unwrap();
            let sub = resolve_subject_with(&root, &n.to_string(), true).unwrap();
            assert_eq!(sub.postop_modality.as_deref(), Some("MRI"), "{key}");
            let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
            assert_eq!(keys, [key]);
            let sub = resolve_subject_with(&root, &n.to_string(), false).unwrap();
            assert_eq!(sub.postop_modality, None, "{key}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    fn modality_less_row_subject(name: &str, file: &str, entities: Value) -> (PathBuf, Subject) {
        let root = scratch(name);
        touch(&root.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"));
        touch(&root.join(format!("rawdata/sub-1/ses-postop/anat/{file}")));
        std::fs::write(
            root.join("rawdata/sub-1/rawimages.json"),
            json!([{"path": format!("rawdata/sub-1/ses-postop/anat/{file}"),
                    "session": "postop", "datatype": "anat",
                    "entities": entities}])
            .to_string(),
        )
        .unwrap();
        let sub = resolve_subject_with(&root, "1", true).unwrap();
        (root, sub)
    }

    #[test]
    fn a_modality_less_row_with_acq_ax_is_an_axial_mr() {
        let (root, sub) = modality_less_row_subject(
            "nomod_ax_acq",
            "sub-1_ses-postop_T2w.nii.gz",
            json!({"suffix": "T2w", "acq": "ax"}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["ax_MRI"]);
        assert!(sub.postop_ct_raw.is_none());
        assert_eq!(sub.postop_modality.as_deref(), Some("MRI"));
        assert!(!sub.both_mrct_present);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_modality_less_row_takes_the_mr_orientation_from_its_acq_entity() {
        let (root, sub) = modality_less_row_subject(
            "nomod_cor",
            "sub-1_ses-postop_T2w.nii.gz",
            json!({"suffix": "T2w", "acq": "cor"}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["cor_MRI"]);
        assert_eq!(sub.postop_modality.as_deref(), Some("MRI"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_modality_less_row_with_suffix_ct_is_the_ct() {
        let (root, sub) = modality_less_row_subject(
            "nomod_ct",
            "sub-1_ses-postop_head.nii.gz",
            json!({"suffix": "CT"}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["CT"]);
        assert!(sub.postop_ct_raw.is_some());
        assert_eq!(sub.postop_modality.as_deref(), Some("CT"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_modality_less_row_without_ct_suffix_or_acq_is_an_axial_mr() {
        let (root, sub) = modality_less_row_subject(
            "nomod_plain",
            "sub-1_ses-postop_T2w.nii.gz",
            json!({"suffix": "T2w"}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["ax_MRI"]);
        assert!(sub.postop_ct_raw.is_none());
        assert_eq!(sub.postop_modality.as_deref(), Some("MRI"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_null_modality_counts_as_absent() {
        let (root, sub) = modality_less_row_subject(
            "nomod_null",
            "sub-1_ses-postop_head.nii.gz",
            json!({"modality": null, "suffix": "CT"}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["CT"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_padded_upper_case_acq_is_matched() {
        let (root, sub) = modality_less_row_subject(
            "nomod_padacq",
            "sub-1_ses-postop_T2w.nii.gz",
            json!({"suffix": "T2w", "acq": " COR "}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["cor_MRI"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lower_case_ct_suffix_is_the_ct() {
        let (root, sub) = modality_less_row_subject(
            "nomod_lowct",
            "sub-1_ses-postop_head.nii.gz",
            json!({"suffix": "ct"}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["CT"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_modality_counts_as_absent() {
        let (root, sub) = modality_less_row_subject(
            "nomod_empty",
            "sub-1_ses-postop_T2w.nii.gz",
            json!({"modality": "", "suffix": "T2w", "acq": "sag"}),
        );
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["sag_MRI"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_logical_index_key_names_the_mr_bundle_unless_the_file_name_does() {
        assert_eq!(
            postop_bundle_key("cor_MRI", "sub-1_ses-postop_T2w.nii.gz"),
            "cor_MRI"
        );
        assert_eq!(
            postop_bundle_key("sag_MRI", "sub-1_ses-postop_T2w.nii.gz"),
            "sag_MRI"
        );
        assert_eq!(
            postop_bundle_key("ax_MRI", "sub-1_ses-postop_T2w.nii.gz"),
            "ax_MRI"
        );
        assert_eq!(
            postop_bundle_key("ax_MRI", "sub-1_ses-postop_acq-sag_T2w.nii.gz"),
            "sag_MRI"
        );
        assert_eq!(
            postop_bundle_key("cor_MRI", "sub-1_ses-postop_acq-ax_T2w.nii.gz"),
            "ax_MRI"
        );
    }

    #[test]
    fn postop_modality_follows_the_bundles_from_the_index() {
        let root = scratch("ctindex");
        touch(&root.join("rawdata/sub-4/ses-preop/anat/sub-4_ses-preop_T1w.nii.gz"));
        let post = root.join("rawdata/sub-4/ses-postop/anat");
        touch(&post.join("sub-4_ses-postop_head.nii.gz"));
        touch(&post.join("sub-4_ses-postop_tse.nii.gz"));
        let row = |file: &str, suffix: &str, modality: &str| {
            json!({"path": format!("rawdata/sub-4/ses-postop/anat/{file}"),
                   "session": "postop", "datatype": "anat",
                   "entities": {"suffix": suffix, "modality": modality}})
        };
        let index = root.join("rawdata/sub-4/rawimages.json");
        std::fs::write(
            &index,
            json!([row("sub-4_ses-postop_head.nii.gz", "CT", "CT")]).to_string(),
        )
        .unwrap();
        let sub = resolve_subject_with(&root, "4", true).unwrap();
        assert!(sub.postop_ct_raw.is_some());
        assert_eq!(sub.postop_modality.as_deref(), Some("CT"));
        assert!(!sub.both_mrct_present);
        assert!(sub
            .postop_raw
            .get("ax_MRI")
            .unwrap()
            .ends_with("sub-4_ses-postop_tse.nii.gz"));
        std::fs::write(
            &index,
            json!([
                row("sub-4_ses-postop_head.nii.gz", "CT", "CT"),
                row("sub-4_ses-postop_tse.nii.gz", "T2w", "ax_MRI")
            ])
            .to_string(),
        )
        .unwrap();
        let sub = resolve_subject_with(&root, "4", true).unwrap();
        assert_eq!(sub.postop_modality.as_deref(), Some("CT"));
        assert!(sub.both_mrct_present);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_choice_ignores_case_and_postop_keeps_insertion_order() {
        let root = scratch("case");
        let anat = root.join("rawdata/sub-5/ses-preop/anat");
        touch(&anat.join("sub-5_ses-preop_acq-B_T1w.nii.gz"));
        touch(&anat.join("sub-5_ses-preop_acq-a_T1w.nii.gz"));
        let post = root.join("rawdata/sub-5/ses-postop/anat");
        touch(&post.join("sub-5_ses-postop_acq-sag_T1w.nii.gz"));
        touch(&post.join("sub-5_ses-postop_ct.nii.gz"));
        let sub = resolve_subject_with(&root, "5", false).unwrap();
        assert!(sub
            .preop_anchor_raw
            .unwrap()
            .ends_with("sub-5_ses-preop_acq-a_T1w.nii.gz"));
        assert_eq!(sub.postop_modality.as_deref(), Some("CT"));
        assert!(sub.both_mrct_present);
        let keys: Vec<&str> = sub.postop_raw.keys().map(String::as_str).collect();
        assert_eq!(keys, ["sag_MRI", "CT"]);
        let ids = list_subject_ids(&root);
        assert_eq!(ids, ["5"]);
        std::fs::create_dir_all(root.join("derivatives/sub-6")).unwrap();
        assert_eq!(list_subject_ids(&root), ["5", "6"]);
        let out = root.join("out");
        std::fs::create_dir_all(out.join("sub-9")).unwrap();
        std::fs::create_dir_all(root.join("derivatives/leaddbs/sub-8")).unwrap();
        assert_eq!(list_subject_ids(&root), ["5", "6", "8"]);
        assert_eq!(list_subject_ids_in(&root, Some(&out)), ["5", "6", "9"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_chosen_output_root_holds_the_derivatives_and_the_prefs_index() {
        let root = scratch("outroot");
        let out = root.join("results");
        touch(&root.join("rawdata/sub-4/ses-preop/anat/sub-4_ses-preop_T1w.nii.gz"));
        let sub = resolve_subject_in(&root, Some(&out), "4", true).unwrap();
        let out_abs = crate::paths::absolute(&out);
        assert_eq!(sub.subj_dir, out_abs.join("sub-4"));
        assert_eq!(sub.locks_dir(), out_abs.join("sub-4").join("locks"));
        assert!(sub.preproc_dir.starts_with(&out_abs));
        assert_eq!(
            sub.rawdata_dir,
            crate::paths::absolute(&root).join("rawdata").join("sub-4")
        );
        let default = resolve_subject_with(&root, "4", true).unwrap();
        assert!(default.subj_dir.ends_with("derivatives/leaddbs/sub-4"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn partial_outputs_are_not_discovered() {
        let root = scratch("partial");
        touch(&root.join("rawdata/sub-7/ses-preop/anat/sub-7_ses-preop_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "7", false).unwrap();
        touch(
            &sub.preproc_dir
                .join(".partial-sub-7_ses-preop_desc-preproc_T1w.nii.gz"),
        );
        let sub = resolve_subject_with(&root, "7", false).unwrap();
        assert!(sub.preop_anchor_preproc.is_none());
        assert!(sub.norm_postop_images().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_n4_output_becomes_the_anchor() {
        let root = scratch("n4");
        touch(&root.join("rawdata/sub-4/ses-preop/anat/sub-4_ses-preop_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "4", false).unwrap();
        assert!(sub.preop_anchor_preproc.is_none());
        let n4 = crate::derivatives::preproc_image(&sub);
        assert!(n4.ends_with("sub-4_ses-preop_desc-preproc_T1w.nii.gz"));
        touch(&n4);
        let sub = resolve_subject_with(&root, "4", false).unwrap();
        assert_eq!(sub.anchor_best(), Some(n4.as_path()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn post_op_mr_images_come_in_ax_cor_sag_order_and_the_norm_choice_follows_it() {
        let root = scratch("mrorder");
        touch(&root.join("rawdata/sub-7/ses-preop/anat/sub-7_ses-preop_T1w.nii.gz"));
        let post = root.join("rawdata/sub-7/ses-postop/anat");
        touch(&post.join("sub-7_ses-postop_acq-sag_T1w.nii.gz"));
        touch(&post.join("sub-7_ses-postop_acq-ax_T1w.nii.gz"));
        touch(&post.join("sub-7_ses-postop_acq-cor_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "7", false).unwrap();
        let keys: Vec<&str> = sub.postop_mr_images().iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["ax_MRI", "cor_MRI", "sag_MRI"]);
        assert!(sub.norm_postop_mr_in_use().is_none());
        let space = "MNI152NLin2009bAsym";
        std::fs::create_dir_all(crate::derivatives::norm_log_dir(&sub)).unwrap();
        std::fs::write(
            crate::derivatives::norm_method_log(&sub),
            format!(r#"{{"template_space": "{space}"}}"#),
        )
        .unwrap();
        for key in ["sag_MRI", "cor_MRI", "ax_MRI"] {
            let want = crate::derivatives::norm_postop_image(&sub, key, space);
            touch(&want);
            let mut fresh = sub.clone();
            fresh.refresh_bundles();
            assert!(fresh.norm_postop_mr_in_use().is_some());
            assert_eq!(fresh.norm.anat_postop.get(key), Some(&want));
        }
        let mut fresh = sub.clone();
        fresh.refresh_bundles();
        assert_eq!(
            fresh.norm_postop_mr_in_use(),
            Some(crate::derivatives::norm_postop_image(&sub, "ax_MRI", space))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_coreg_bundle_lists_the_images_and_transforms_that_exist() {
        use crate::derivatives as d;
        let root = scratch("coreg_bundle");
        touch(&root.join("rawdata/sub-6/ses-preop/anat/sub-6_ses-preop_T1w.nii.gz"));
        touch(&root.join("rawdata/sub-6/ses-preop/anat/sub-6_ses-preop_T2w.nii.gz"));
        touch(&root.join("rawdata/sub-6/ses-postop/anat/sub-6_ses-postop_CT.nii.gz"));
        touch(&root.join("rawdata/sub-6/ses-postop/anat/sub-6_ses-postop_acq-ax_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "6", false).unwrap();
        assert!(sub.coreg.anat_postop.is_empty() && sub.coreg.transform.is_empty());
        assert!(sub.coreg.log_method.is_none() && sub.norm.log_method.is_none());
        touch(&d::coreg_ct_image(&sub));
        touch(&d::coreg_ct_affine(&sub));
        touch(&d::coreg_mr_image(&sub, "ax_MRI"));
        touch(&d::coreg_t2_image(&sub));
        touch(&d::coreg_t2_affine(&sub));
        touch(&d::coreg_method_log(&sub));
        touch(&d::precoreg_image(&sub));
        let sub = resolve_subject_with(&root, "6", false).unwrap();
        assert_eq!(
            sub.coreg.anat_postop.get("CT"),
            Some(&d::coreg_ct_image(&sub))
        );
        assert_eq!(
            sub.coreg.anat_postop.get("ax_MRI"),
            Some(&d::coreg_mr_image(&sub, "ax_MRI"))
        );
        assert_eq!(
            sub.coreg.anat_preop.get("T2w"),
            Some(&d::coreg_t2_image(&sub))
        );
        assert_eq!(
            sub.coreg.anat_preop.get("T1w"),
            Some(&d::precoreg_image(&sub))
        );
        assert_eq!(
            sub.coreg.transform.get("CT"),
            Some(&d::coreg_ct_affine(&sub))
        );
        assert_eq!(
            sub.coreg.transform.get("T2w"),
            Some(&d::coreg_t2_affine(&sub))
        );
        assert!(!sub.coreg.transform.contains_key("ax_MRI"));
        assert_eq!(sub.coreg.log_method, Some(d::coreg_method_log(&sub)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_anchor_in_use_is_precoreg_then_n4_then_raw() {
        let root = scratch("anchor_in_use");
        touch(&root.join("rawdata/sub-8/ses-preop/anat/sub-8_ses-preop_T1w.nii.gz"));
        let sub = resolve_subject_with(&root, "8", false).unwrap();
        let name = |p: Option<PathBuf>| {
            p.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        };
        assert_eq!(
            name(sub.anchor_in_use()).as_deref(),
            Some("sub-8_ses-preop_T1w.nii.gz")
        );
        touch(&crate::derivatives::preproc_image(&sub));
        let sub = resolve_subject_with(&root, "8", false).unwrap();
        assert_eq!(
            name(sub.anchor_in_use()).as_deref(),
            Some("sub-8_ses-preop_desc-preproc_T1w.nii.gz")
        );
        let precoreg = crate::derivatives::precoreg_image(&sub);
        touch(&precoreg);
        assert_eq!(sub.anchor_in_use(), Some(precoreg));
        assert_eq!(
            name(sub.anchor_best().map(Path::to_path_buf)).as_deref(),
            Some("sub-8_ses-preop_desc-preproc_T1w.nii.gz")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod verbatim_tests {
    use super::simplify_verbatim;
    use std::path::PathBuf;

    /// Pure string work, so it is asserted on literals and runs on every platform — the bug it
    /// prevents only appears on Windows, where nobody may be looking.
    #[test]
    fn drive_and_unc_prefixes_are_shortened() {
        assert_eq!(
            simplify_verbatim(PathBuf::from(r"\\?\C:\Users\x\data")),
            PathBuf::from(r"C:\Users\x\data")
        );
        assert_eq!(
            simplify_verbatim(PathBuf::from(r"\\?\UNC\server\share\data")),
            PathBuf::from(r"\\server\share\data")
        );
    }

    #[test]
    fn other_paths_are_untouched() {
        for p in [
            "/home/x/data",
            r"C:\Users\x\data",
            r"\\server\share",
            r"\\?\Volume{deadbeef}\x",
            "relative/path",
            "",
        ] {
            assert_eq!(
                simplify_verbatim(PathBuf::from(p)),
                PathBuf::from(p),
                "{p} was altered"
            );
        }
    }

    /// The point of the fix: one file, one string identity. Anything path-keyed — the stage
    /// locks, the scene manifest — must agree between code paths.
    #[test]
    fn shortening_is_idempotent() {
        let once = simplify_verbatim(PathBuf::from(r"\\?\C:\Users\x\data"));
        assert_eq!(simplify_verbatim(once.clone()), once);
    }
}
