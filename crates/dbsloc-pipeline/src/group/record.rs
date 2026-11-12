// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The group record of `specs/14` §2.3: types, JSON load / save, schema check, defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::clinical::ClinicalVar;
use super::GroupError;
use crate::outputs::write_replacing;
use crate::vat::settings::ModelKind;
use crate::version::{software_stamp, utc_now_rfc3339};

/// Schema id of the group record JSON.
pub const SCHEMA: &str = "dbsloc-group/1";
/// Number of default group colours.
pub const PALETTE_LEN: usize = 36;

const PALETTE: [&str; PALETTE_LEN] = [
    "1DACE8", "1C366B", "F24D29", "E5C4A1", "C4CFD0", "D8A49B", "C7CEF6", "7496D2", "B62A3D",
    "EDCB64", "B5966D", "DAECED", "CECD7B", "9A872D", "F5CDB6", "F7B0AA", "FDDDA4", "76A08A",
    "F8DF4F", "A35E60", "541F12", "CC8B3C", "E8D2B9", "AEA8A8", "CB9E23", "957A6D", "AC6E49",
    "456355", "FCD16B", "D3DDDC", "C6B19D", "DBB165", "DEB18B", "2E604A", "27223C", "D1362F",
];

fn yes() -> bool {
    true
}

fn one() -> u32 {
    1
}

fn schema_name() -> String {
    SCHEMA.to_owned()
}

fn per_subject() -> String {
    "per_subject".to_owned()
}

/// One dataset the patients come from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetRef {
    pub id: String,
    pub root: String,
    pub derivatives_root: String,
}

/// One patient of the group; the order of the list is the Lead Group patient order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatientEntry {
    pub subject: String,
    pub dataset: String,
    #[serde(default = "one")]
    pub group: u32,
    #[serde(default)]
    pub elmodel_override: Option<String>,
    #[serde(default = "yes")]
    pub included: bool,
}

/// A group (cohort number) with its label and optional explicit colour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupDef {
    pub number: u32,
    pub label: String,
    #[serde(default)]
    pub color: Option<[f64; 3]>,
}

/// The stimulation label of the group and the VTA model it uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StimulationRef {
    pub label: String,
    #[serde(default)]
    pub model: Option<ModelKind>,
    #[serde(default = "per_subject")]
    pub settings_ref: String,
}

/// The `M.ui` fields we model (`specs/14` §2.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    pub list_select: Vec<u32>,
    pub clinical_list: Vec<u32>,
    pub highlight_active: bool,
    pub show_passive: bool,
    pub show_active: bool,
    pub show_isovolume: bool,
    pub isovolume_display: u32,
    pub elmodel_select: u32,
    pub normalisation: u32,
    pub colour_contacts: bool,
    pub mirror_sides: bool,
    pub colour_by: Vec<String>,
    pub parcellation: Option<String>,
    pub connectome: Option<String>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            list_select: vec![1],
            clinical_list: vec![1],
            highlight_active: false,
            show_passive: true,
            show_active: true,
            show_isovolume: false,
            isovolume_display: 1,
            elmodel_select: 1,
            normalisation: 1,
            colour_contacts: false,
            mirror_sides: false,
            colour_by: Vec::new(),
            parcellation: None,
            connectome: None,
        }
    }
}

/// Kind, folder and record file name of the sweet-spot, fibre-filtering and network-mapping analyses.
pub const EXPLORER_LAYOUTS: [(&str, &str, &str); 3] = [
    ("sweetspot", "sweetspots", "sweetspot.json"),
    ("fiberfiltering", "fiberfiltering", "fibrefilter.json"),
    ("networkmapping", "networkmapping", "netmap.json"),
];

/// A group analysis stored under the group folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisRef {
    pub kind: String,
    pub id: String,
    pub record: String,
}

/// State of the Lead Group twin: its hash, carried fields and fields that could not be decoded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LeadGroupInfo {
    #[serde(default)]
    pub mat_file: String,
    #[serde(default)]
    pub mat_sha256: String,
    #[serde(default)]
    pub passthrough: Map<String, Value>,
    #[serde(default)]
    pub dropped_fields: Vec<String>,
}

/// The group record (`dbsloc-group/1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupRecord {
    #[serde(default = "schema_name")]
    pub schema: String,
    pub guid: String,
    pub dataset_name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub software: Value,
    #[serde(default)]
    pub datasets: Vec<DatasetRef>,
    #[serde(default)]
    pub patients: Vec<PatientEntry>,
    #[serde(default)]
    pub groups: Vec<GroupDef>,
    #[serde(default)]
    pub stimulation: Option<StimulationRef>,
    #[serde(default)]
    pub clinical: Vec<ClinicalVar>,
    #[serde(default)]
    pub atlas_set: Option<String>,
    #[serde(default)]
    pub view: ViewState,
    #[serde(default)]
    pub analyses: Vec<AnalysisRef>,
    #[serde(default)]
    pub lead_group: LeadGroupInfo,
}

/// `true` when `s` is non-empty and made of ASCII letters and digits only.
pub fn is_alphanumeric_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The stimulation label of a group, `gs_<guid>`.
pub fn group_label(guid: &str) -> String {
    format!("gs_{guid}")
}

/// `dataset-<dataset>_analysis-<guid>`, the stem of every group file name.
pub fn file_stem(dataset_name: &str, guid: &str) -> String {
    format!("dataset-{dataset_name}_analysis-{guid}")
}

/// `<group folder>/dataset-<d>_analysis-<guid>.json`.
pub fn record_path(folder: &Path, dataset_name: &str, guid: &str) -> PathBuf {
    folder.join(format!("{}.json", file_stem(dataset_name, guid)))
}

/// `<group folder>/dataset-<d>_analysis-<guid>.mat`, the Lead Group twin.
pub fn twin_path(folder: &Path, dataset_name: &str, guid: &str) -> PathBuf {
    folder.join(format!("{}.mat", file_stem(dataset_name, guid)))
}

/// `<group folder>/dataset-<d>_analysis-<guid>_desc-backup<utc>.mat`.
pub fn backup_path(folder: &Path, dataset_name: &str, guid: &str, utc_digits: &str) -> PathBuf {
    folder.join(format!(
        "{}_desc-backup{utc_digits}.mat",
        file_stem(dataset_name, guid)
    ))
}

/// The digits of an RFC 3339 timestamp (`yyyymmddHHMMSS` for a UTC time).
pub fn digits_of(timestamp: &str) -> String {
    timestamp.chars().filter(char::is_ascii_digit).collect()
}

/// `yyyymmddHHMMSS` of the current UTC time.
pub fn utc_digits_now() -> String {
    digits_of(&utc_now_rfc3339())
}

/// Colour of the group at rank `number` (1-based): the 36-colour palette with the saturation halved.
pub fn default_color(number: u32) -> [f64; 3] {
    let index = (number.max(1) as usize - 1) % PALETTE_LEN;
    let hex = PALETTE[index];
    let channel = |i: usize| {
        f64::from(u8::from_str_radix(hex.get(2 * i..2 * i + 2).unwrap_or("00"), 16).unwrap_or(0))
            / 255.0
    };
    let (h, s, v) = rgb_to_hsv(channel(0), channel(1), channel(2));
    hsv_to_rgb(h, s * 0.5, v)
}

fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let i = h6.floor();
    let f = h6 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i as u32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Splits on `/` and `\` and returns `(parent, last component)`.
pub fn split_last(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => ("", trimmed),
    }
}

/// Joins a name to a folder string with the separator the folder already uses.
pub fn join_native(root: &str, name: &str) -> String {
    let root = root.trim_end_matches(['/', '\\']);
    let sep = if root.contains('\\') && !root.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{root}{sep}{name}")
}

/// Dataset root of a derivatives root (`<dataset>/derivatives/<pipeline>` gives `<dataset>`).
pub fn dataset_root_of(derivatives_root: &str) -> String {
    let (parent, _) = split_last(derivatives_root);
    let (grand, name) = split_last(parent);
    if name == "derivatives" {
        grand.to_owned()
    } else {
        parent.to_owned()
    }
}

/// The dataset folder name in front of `derivatives` in a group folder path.
pub fn dataset_name_of_folder(folder: &Path) -> Option<String> {
    let parts: Vec<String> = folder
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let at = parts.iter().rposition(|p| p == "derivatives")?;
    at.checked_sub(1).map(|i| parts[i].clone())
}

/// Subject name with the `sub-` prefix.
pub fn subject_name(subject: &str) -> String {
    if subject.starts_with("sub-") {
        subject.to_owned()
    } else {
        format!("sub-{subject}")
    }
}

/// Subject id without the `sub-` prefix.
pub fn subject_id(subject: &str) -> &str {
    subject.strip_prefix("sub-").unwrap_or(subject)
}

impl GroupRecord {
    /// A new empty record with the stimulation label `gs_<guid>`.
    pub fn new(guid: &str, dataset_name: &str) -> GroupRecord {
        let now = utc_now_rfc3339();
        GroupRecord {
            schema: schema_name(),
            guid: guid.to_owned(),
            dataset_name: dataset_name.to_owned(),
            title: String::new(),
            created_at: now.clone(),
            updated_at: now,
            software: software_stamp(),
            datasets: Vec::new(),
            patients: Vec::new(),
            groups: Vec::new(),
            stimulation: Some(StimulationRef {
                label: group_label(guid),
                model: None,
                settings_ref: per_subject(),
            }),
            clinical: Vec::new(),
            atlas_set: None,
            view: ViewState::default(),
            analyses: Vec::new(),
            lead_group: LeadGroupInfo::default(),
        }
    }

    /// Checks the schema id, the guid and the shape of every clinical variable.
    pub fn validate(&self) -> Result<(), GroupError> {
        if self.schema != SCHEMA {
            return Err(GroupError::Invalid(format!(
                "unsupported group record schema {:?} (expected {SCHEMA})",
                self.schema
            )));
        }
        if !is_alphanumeric_id(&self.guid) {
            return Err(GroupError::Invalid(format!(
                "guid {:?} must be made of letters and digits only",
                self.guid
            )));
        }
        for v in &self.clinical {
            if v.values.len() != self.patients.len() {
                return Err(GroupError::Invalid(format!(
                    "clinical variable {:?} has {} rows for {} patients",
                    v.name,
                    v.values.len(),
                    self.patients.len()
                )));
            }
        }
        for p in &self.patients {
            if !self.datasets.iter().any(|d| d.id == p.dataset) {
                return Err(GroupError::Invalid(format!(
                    "patient {} refers to unknown dataset {:?}",
                    p.subject, p.dataset
                )));
            }
        }
        Ok(())
    }

    /// Lists analysis `id` of `kind` stored as `<folder>/<id>/<json_name>` under the group folder.
    pub fn register_analysis(&mut self, kind: &str, folder: &str, id: &str, json_name: &str) {
        self.analyses.push(AnalysisRef {
            kind: kind.to_owned(),
            id: id.to_owned(),
            record: format!("{folder}/{id}/{json_name}"),
        });
    }

    /// The label `gs_<guid>`.
    pub fn stim_label(&self) -> String {
        group_label(&self.guid)
    }

    /// Id of the dataset with this derivatives root, added when new.
    pub fn ensure_dataset(&mut self, root: &str, derivatives_root: &str) -> String {
        let norm = |s: &str| s.trim_end_matches(['/', '\\']).to_owned();
        let target = norm(derivatives_root);
        if let Some(d) = self
            .datasets
            .iter()
            .find(|d| norm(&d.derivatives_root) == target)
        {
            return d.id.clone();
        }
        let mut n = self.datasets.len() + 1;
        while self.datasets.iter().any(|d| d.id == format!("d{n}")) {
            n += 1;
        }
        let id = format!("d{n}");
        self.datasets.push(DatasetRef {
            id: id.clone(),
            root: norm(root),
            derivatives_root: target,
        });
        id
    }

    /// Index of the patient `subject` (with or without `sub-`).
    pub fn patient_index(&self, subject: &str) -> Option<usize> {
        let want = subject_name(subject);
        self.patients
            .iter()
            .position(|p| subject_name(&p.subject) == want)
    }

    /// Adds a patient with NaN rows in every clinical variable; a listed subject is refused.
    pub fn add_patient(
        &mut self,
        subject: &str,
        dataset: &str,
        group: u32,
    ) -> Result<(), GroupError> {
        let name = subject_name(subject);
        if self.patient_index(&name).is_some() {
            return Err(GroupError::Invalid(format!(
                "{name} is already in the group"
            )));
        }
        if group == 0 {
            return Err(GroupError::Invalid("group numbers start at 1".to_owned()));
        }
        self.patients.push(PatientEntry {
            subject: name,
            dataset: dataset.to_owned(),
            group,
            elmodel_override: None,
            included: true,
        });
        for v in &mut self.clinical {
            let w = v.width();
            v.values.push(vec![None; w]);
        }
        self.ensure_groups();
        Ok(())
    }

    /// Removes a patient and its row of every clinical variable; `false` when it is not listed.
    pub fn remove_patient(&mut self, subject: &str) -> bool {
        let Some(i) = self.patient_index(subject) else {
            return false;
        };
        self.patients.remove(i);
        for v in &mut self.clinical {
            if i < v.values.len() {
                v.values.remove(i);
            }
        }
        let n = self.patients.len();
        self.view.list_select.retain(|s| (*s as usize) <= n);
        self.ensure_groups();
        true
    }

    /// Sets the group number of a patient.
    pub fn set_group(&mut self, subject: &str, group: u32) -> Result<(), GroupError> {
        if group == 0 {
            return Err(GroupError::Invalid("group numbers start at 1".to_owned()));
        }
        let i = self
            .patient_index(subject)
            .ok_or_else(|| GroupError::Invalid(format!("{subject} is not in the group")))?;
        self.patients[i].group = group;
        self.ensure_groups();
        Ok(())
    }

    /// Keeps exactly one group definition per group number in use, sorted by number.
    pub fn ensure_groups(&mut self) {
        let mut numbers: Vec<u32> = self.patients.iter().map(|p| p.group).collect();
        numbers.sort_unstable();
        numbers.dedup();
        let defs: Vec<GroupDef> = numbers
            .into_iter()
            .map(|n| {
                self.groups
                    .iter()
                    .find(|g| g.number == n)
                    .cloned()
                    .unwrap_or_else(|| GroupDef {
                        number: n,
                        label: format!("Group {n}"),
                        color: None,
                    })
            })
            .collect();
        self.groups = defs;
    }

    /// The label of group `number`.
    pub fn group_label_of(&self, number: u32) -> String {
        self.groups
            .iter()
            .find(|g| g.number == number)
            .map_or_else(|| format!("Group {number}"), |g| g.label.clone())
    }

    /// The derivatives root string of the patient's dataset.
    pub fn derivatives_root_of(&self, patient: &PatientEntry) -> Option<&str> {
        self.datasets
            .iter()
            .find(|d| d.id == patient.dataset)
            .map(|d| d.derivatives_root.as_str())
    }

    /// The patient's `<derivatives root>/sub-<id>` folder.
    pub fn patient_folder(&self, patient: &PatientEntry) -> Option<PathBuf> {
        self.derivatives_root_of(patient)
            .map(|root| PathBuf::from(join_native(root, &patient.subject)))
    }
}

/// Reads and validates a group record.
pub fn load_record(path: &Path) -> Result<GroupRecord, GroupError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| GroupError::Io(format!("cannot read {}: {e}", path.display())))?;
    let record: GroupRecord = serde_json::from_str(&text)
        .map_err(|e| GroupError::Json(format!("{}: {e}", path.display())))?;
    record.validate()?;
    Ok(record)
}

/// Stamps `updated_at` and the software block and writes the record atomically.
pub fn save_record(path: &Path, record: &mut GroupRecord) -> Result<(), GroupError> {
    record.validate()?;
    record.updated_at = utc_now_rfc3339();
    record.software = software_stamp();
    let text = serde_json::to_string_pretty(record)
        .map(|t| t + "\n")
        .map_err(|e| GroupError::Json(e.to_string()))?;
    write_replacing(path, text.as_bytes())
        .map_err(|e| GroupError::Io(format!("cannot write {}: {e}", path.display())))
}

/// The group record file for a path that is the JSON file or its folder.
pub fn find_record_file(path: &Path) -> Result<PathBuf, GroupError> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.is_dir() {
        return Err(GroupError::Io(format!("{} does not exist", path.display())));
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| GroupError::Io(format!("cannot read {}: {e}", path.display())))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with("dataset-") && n.contains("_analysis-") && n.ends_with(".json")
            })
        })
        .collect();
    found.sort();
    match found.len() {
        0 => Err(GroupError::Io(format!(
            "no dataset-*_analysis-*.json group file in {}",
            path.display()
        ))),
        1 => Ok(found.remove(0)),
        _ => Err(GroupError::Invalid(format!(
            "several group files in {}; name one of them",
            path.display()
        ))),
    }
}
