// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The Lead Group analysis file (`specs/14` §2.2, §2.4–§2.7): reading `M`, writing the MAT v7
//! twin, backing up a foreign twin and importing edits made in Lead Group.

use std::path::{Path, PathBuf};

use fastwarp_matfile::mat5::{mat5_bytes, Mat5Value};
use serde_json::{Map, Value};

use super::clinical::ClinicalVar;
use super::ea_stats::{read_ea_stats, stats_path, EaStats};
use super::matio::{
    approx_bytes, cell_texts, column, field_names, first_number, from_json, numbers, read_variable,
    struct_elements, text, text_cell, to_json,
};
use super::record::{
    backup_path, dataset_name_of_folder, dataset_root_of, default_color, digits_of, group_label,
    is_alphanumeric_id, join_native, split_last, subject_id, twin_path, utc_digits_now, GroupDef,
    GroupRecord, PatientEntry, StimulationRef, ViewState,
};
use super::{GroupError, Note};
use crate::outputs::write_replacing;
use crate::reco_mat::reco_mat_from_json;
use crate::sha256::{file_sha256_hex, sha256_hex};
use crate::vat::settings::{
    from_lead_dbs_s, load as load_stimulation, to_lead_dbs_s, ModelKind, Stimulation,
};
use crate::version::utc_now_rfc3339;

/// Name of the variable of a Lead Group analysis file.
pub const M_VARIABLE: &str = "M";
/// Size above which a v7 variable cannot be written.
pub const V7_LIMIT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_ATLAS_SET: &str = "DISTAL Minimal (Ewert 2017)";
const DEFAULT_CONNECTOME: &str = "Do not calculate connectivity stats";
const DEFAULT_ELMODEL: &str = "Medtronic 3389";
const FALLBACK_CONTACTS: usize = 8;
const KNOWN_TOP: [&str; 16] = [
    "patient",
    "guid",
    "clinical",
    "vilist",
    "fclist",
    "ui",
    "S",
    "vatmodel",
    "root",
    "groups",
    "elstruct",
    "isomatrix",
    "isomatrix_name",
    "stats",
    "stimparams",
    "merstruct",
];
const KNOWN_UI: [&str; 14] = [
    "listselect",
    "clinicallist",
    "hlactivecontcheck",
    "showpassivecontcheck",
    "showactivecontcheck",
    "showisovolumecheck",
    "isovscloudpopup",
    "elmodelselect",
    "normregpopup",
    "colorpointcloudcheck",
    "mirrorsides",
    "atlassetpopup",
    "labelpopup",
    "connectomename",
];

/// A record read from `M`, the stimulation settings of each patient and the notes of the read.
#[derive(Debug, Clone, PartialEq)]
pub struct MRead {
    pub record: GroupRecord,
    pub stimulations: Vec<Option<Stimulation>>,
    pub warnings: Vec<Note>,
}

/// Environment of a read that is not in the file.
#[derive(Debug, Clone, Default)]
pub struct ReadEnv {
    pub atlas_folders: Option<Vec<String>>,
}

impl ReadEnv {
    /// The atlas folders of the Lead-DBS installation named by `DBSLOC_LEADDBS_ATLASES_DIR`.
    pub fn from_environment() -> ReadEnv {
        let folders = crate::atlas_catalog::external_atlases_dir_from_env().and_then(|dir| {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .ok()?
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort_by_key(|n| n.to_lowercase());
            Some(names)
        });
        ReadEnv {
            atlas_folders: folders,
        }
    }
}

fn field<'a>(v: &'a Mat5Value, name: &str) -> Option<&'a Mat5Value> {
    v.field(name, 0)
}

fn has_dropped(dropped: &[String], path: &str) -> bool {
    dropped.iter().any(|d| {
        d.strip_prefix(path)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '{', '(']))
    })
}

fn pass(out: &mut Map<String, Value>, dropped: &[String], key: &str, mpath: &str, v: &Mat5Value) {
    if has_dropped(dropped, mpath) {
        return;
    }
    out.insert(key.to_owned(), to_json(v));
}

fn lg_error(code: &'static str, message: impl Into<String>) -> GroupError {
    GroupError::Lg {
        code,
        message: message.into(),
    }
}

fn file_id_and_dataset(path: &Path) -> (Option<String>, Option<String>) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let Some(stem) = name.as_deref().and_then(|n| n.strip_suffix(".mat")) else {
        return (None, None);
    };
    let Some(rest) = stem.strip_prefix("dataset-") else {
        return (None, None);
    };
    match rest.rsplit_once("_analysis-") {
        Some((dataset, id)) if is_alphanumeric_id(id) => {
            (Some(id.to_owned()), Some(dataset.to_owned()))
        }
        _ => (None, None),
    }
}

fn mtime_digits(path: &Path) -> String {
    let secs = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    match secs {
        Some(s) => digits_of(&crate::version::utc_rfc3339(s)),
        None => utc_digits_now(),
    }
}

fn to_u32(v: f64) -> u32 {
    if v.is_finite() && v > 0.0 {
        v.round() as u32
    } else {
        0
    }
}

fn indices(v: Option<&Mat5Value>, default: &[u32]) -> Vec<u32> {
    match v {
        Some(v) => numbers(v)
            .into_iter()
            .map(to_u32)
            .filter(|i| *i > 0)
            .collect(),
        None => default.to_vec(),
    }
}

fn flag(v: Option<&Mat5Value>, default: bool) -> bool {
    v.and_then(first_number).map_or(default, |x| x != 0.0)
}

fn popup(v: Option<&Mat5Value>, default: u32) -> u32 {
    v.and_then(first_number).map_or(default, to_u32)
}

fn resolve_named(
    value: Option<&Mat5Value>,
    env: &ReadEnv,
    what: &str,
    warnings: &mut Vec<Note>,
) -> Option<String> {
    let v = value?;
    if let Some(s) = text(v) {
        return Some(s.to_owned());
    }
    let n = first_number(v)?;
    let index = to_u32(n);
    if what == "atlas set" {
        if let Some(name) = env
            .atlas_folders
            .as_ref()
            .and_then(|f| f.get((index as usize).wrapping_sub(1)))
        {
            return Some(name.clone());
        }
    }
    warnings.push(Note::plain(format!(
        "{what} is stored as the index {index} and cannot be resolved without the atlas folders of a Lead-DBS installation (DBSLOC_LEADDBS_ATLASES_DIR); kept as #{index}"
    )));
    Some(format!("#{index}"))
}

fn unresolved_to_mat(name: &str) -> Mat5Value {
    match name.strip_prefix('#').and_then(|n| n.parse::<f64>().ok()) {
        Some(n) => Mat5Value::scalar(n),
        None => Mat5Value::char(name),
    }
}

struct Patients {
    entries: Vec<(String, String)>,
}

fn read_patients(m: &Mat5Value) -> Result<(Patients, Option<&Mat5Value>), GroupError> {
    let patient = field(m, "patient")
        .filter(|p| matches!(p, Mat5Value::Struct { .. }))
        .ok_or_else(|| lg_error("E_LG_NOT_GROUP", "M.patient is missing or not a struct"))?;
    let list = field(patient, "list")
        .and_then(cell_texts)
        .ok_or_else(|| lg_error("E_LG_NOT_GROUP", "M.patient.list is not a cell of char"))?;
    let entries = list
        .iter()
        .map(|p| {
            let (deriv, name) = split_last(p);
            (deriv.to_owned(), name.to_owned())
        })
        .collect();
    Ok((Patients { entries }, Some(patient)))
}

fn remap_root(source: &Path) -> Option<PathBuf> {
    let folder = source.parent()?;
    let leadgroup = folder.parent()?;
    if leadgroup.file_name()? != "leadgroup" {
        return None;
    }
    let derivatives = leadgroup.parent()?;
    if derivatives.file_name()? != "derivatives" {
        return None;
    }
    derivatives.parent().map(Path::to_path_buf)
}

fn is_v73(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 128];
    std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .is_ok_and(|n| String::from_utf8_lossy(&head[..n]).contains("MATLAB 7.3 MAT-file"))
}

fn stats_entry_names(ea: &Mat5Value) -> Option<Vec<String>> {
    field(ea, "atlases")
        .and_then(|a| field(a, "names"))
        .and_then(cell_texts)
}

/// The reasons `M.stats` no longer matches the record (`specs/14` §2.7, rule 9): a different
/// patient count or order, no entry for the group label, or atlas structure names that differ
/// from `M.vilist` (else from the first patient). Missing fields never count as a difference.
fn stats_stale_reasons(m: &Mat5Value, record: &GroupRecord) -> Vec<String> {
    let Some(stats) = field(m, "stats") else {
        return Vec::new();
    };
    let elements = struct_elements(stats);
    if elements.is_empty() {
        return Vec::new();
    }
    let label = record.stim_label();
    let mut reasons = Vec::new();
    if elements.len() != record.patients.len() {
        reasons.push(format!(
            "it holds {} patients, the group lists {}",
            elements.len(),
            record.patients.len()
        ));
    }
    let vilist = field(m, "vilist")
        .and_then(cell_texts)
        .filter(|v| !v.is_empty());
    let mut reference = vilist;
    for (i, el) in elements.iter().enumerate() {
        let Some(ea) = field(el, "ea_stats").filter(|e| matches!(e, Mat5Value::Struct { .. }))
        else {
            continue;
        };
        let patient = record.patients.get(i).map(|p| p.subject.as_str());
        if let (Some(patient), Some(found)) = (
            patient,
            field(ea, "patname")
                .and_then(cell_texts)
                .and_then(|v| v.into_iter().next()),
        ) {
            if subject_id(&found) != subject_id(patient) {
                reasons.push(format!(
                    "entry {} is for {found}, the group lists {patient} there",
                    i + 1
                ));
            }
        }
        let who = patient.map_or_else(|| format!("entry {}", i + 1), str::to_owned);
        if let Some(stim) = field(ea, "stimulation") {
            let has_label = struct_elements(stim)
                .iter()
                .any(|s| field(s, "label").and_then(text) == Some(label.as_str()));
            if !has_label {
                reasons.push(format!("{who} has no entry for {label}"));
            }
        }
        if let Some(names) = stats_entry_names(ea) {
            match &reference {
                Some(r) if *r != names => {
                    reasons.push(format!("the atlas structures of {who} differ"));
                }
                None => reference = Some(names),
                _ => {}
            }
        }
    }
    reasons
}

fn stats_stale_note(m: &Mat5Value, record: &GroupRecord) -> Option<Note> {
    let mut reasons = stats_stale_reasons(m, record);
    if reasons.is_empty() {
        return None;
    }
    let more = reasons.len().saturating_sub(5);
    reasons.truncate(5);
    let mut text = format!("M.stats does not match the group ({}", reasons.join("; "));
    if more > 0 {
        text.push_str(&format!("; and {more} more"));
    }
    text.push_str("); Lead Group recalculates it, run Calculate to refresh the statistics here");
    Some(Note::coded("W_LG_STATS_STALE", text))
}

/// Reads a Lead Group analysis file (MAT v5, v7 or v7.3) into a group record. A twin whose
/// `M.stats` no longer matches the patients, stimulation label or atlas structures of the record
/// gives the warning `W_LG_STATS_STALE`; `M.stats` itself is never imported.
pub fn read_m(path: &Path) -> Result<MRead, GroupError> {
    let decoded = read_variable(path, M_VARIABLE, &["stats", "elstruct"])?
        .ok_or_else(|| lg_error("E_LG_NOT_GROUP", "the file has no variable M"))?;
    let sha = file_sha256_hex(path)
        .map_err(|e| GroupError::Io(format!("cannot read {}: {e}", path.display())))?;
    let mut out = record_from_m(
        &decoded.value,
        path,
        decoded.dropped,
        &ReadEnv::from_environment(),
    )?;
    out.record.lead_group.mat_sha256 = sha;
    if is_v73(path) {
        if let Ok(Some(with_stats)) = read_variable(path, M_VARIABLE, &["elstruct"]) {
            out.warnings
                .extend(stats_stale_note(&with_stats.value, &out.record));
        }
    }
    Ok(out)
}

/// Maps an in-memory `M` to a group record with every read rule of `specs/14` §2.5. `source`
/// names the file the value came from (guid, dataset name and the folder remap use it).
pub fn record_from_m(
    m: &Mat5Value,
    source: &Path,
    dropped: Vec<String>,
    env: &ReadEnv,
) -> Result<MRead, GroupError> {
    if !matches!(m, Mat5Value::Struct { .. }) {
        return Err(lg_error("E_LG_NOT_GROUP", "variable M is not a struct"));
    }
    let mut warnings: Vec<Note> = Vec::new();
    let mut passthrough = Map::new();
    let (patients, patient_struct) = read_patients(m)?;
    let n = patients.entries.len();
    let (file_id, file_dataset) = file_id_and_dataset(source);

    let guid = match field(m, "guid").and_then(text) {
        Some(g) if is_alphanumeric_id(g) => g.to_owned(),
        _ => file_id.unwrap_or_else(|| mtime_digits(source)),
    };

    let mut groups_of: Vec<u32> = vec![1; n];
    let group_field = patient_struct.and_then(|p| field(p, "group"));
    match group_field {
        Some(g) if numbers(g).len() == n => {
            for (slot, v) in groups_of.iter_mut().zip(numbers(g)) {
                *slot = to_u32(v).max(1);
            }
        }
        _ => warnings.push(Note::coded(
            "W_LG_GROUPS",
            "patient.group is missing or has the wrong length; every patient is put in group 1",
        )),
    }
    if let Some(p) = patient_struct {
        for name in field_names(p) {
            if name != "list" && name != "group" {
                if let Some(v) = field(p, name) {
                    pass(
                        &mut passthrough,
                        &dropped,
                        &format!("patient.{name}"),
                        &format!("M.patient.{name}"),
                        v,
                    );
                }
            }
        }
    }

    let mut record = GroupRecord::new(&guid, "dataset");
    let x = remap_root(source);
    let miniset = x.as_ref().is_some_and(|x| x.join("miniset.json").is_file());
    let mut seen: Vec<(String, String)> = Vec::new();
    for (p, (deriv, name)) in patients.entries.iter().enumerate() {
        let mut deriv = deriv.clone();
        let original = join_native(&deriv, name);
        let candidate = x.as_ref().map(|x| x.join("derivatives").join("leaddbs"));
        let candidate_exists = candidate.as_ref().is_some_and(|c| c.join(name).exists());
        let own_exists = Path::new(&original).exists();
        match (&candidate, candidate_exists) {
            (Some(c), true) if miniset || !own_exists => {
                deriv = c.to_string_lossy().into_owned();
            }
            _ if !own_exists => warnings.push(Note::coded(
                "W_LG_PATIENT_MISSING",
                format!("the folder of {name} does not exist: {original}"),
            )),
            _ => {}
        }
        if seen.iter().any(|(d, s)| *d == deriv && s == name) {
            warnings.push(Note::coded(
                "W_LG_DUPLICATE",
                format!("{name} is listed more than once; statistics use the first entry"),
            ));
        }
        seen.push((deriv.clone(), name.clone()));
        let dataset = record.ensure_dataset(&dataset_root_of(&deriv), &deriv);
        record.patients.push(PatientEntry {
            subject: name.clone(),
            dataset,
            group: groups_of[p],
            elmodel_override: None,
            included: true,
        });
    }

    record.dataset_name = file_dataset
        .or_else(|| source.parent().and_then(dataset_name_of_folder))
        .or_else(|| {
            record
                .datasets
                .first()
                .map(|d| split_last(&d.root).1.to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "dataset".to_owned());

    read_groups(m, &mut record, &dropped, &mut passthrough);
    read_clinical(m, &mut record, &dropped, &mut passthrough, &mut warnings);
    warnings.extend(stats_stale_note(m, &record));
    read_ui(
        m,
        &mut record,
        env,
        &dropped,
        &mut passthrough,
        &mut warnings,
    );

    let mut stimulations: Vec<Option<Stimulation>> = vec![None; n];
    if let Some(s) = field(m, "S") {
        let elements = struct_elements(s);
        if !elements.is_empty() && elements.len() != n {
            warnings.push(Note::plain(format!(
                "S has {} elements for {n} patients; the first {} are imported",
                elements.len(),
                elements.len().min(n)
            )));
        }
        let label = group_label(&guid);
        for (p, el) in elements.iter().enumerate().take(n) {
            let id = subject_id(&record.patients[p].subject).to_owned();
            match from_lead_dbs_s(el, &id, FALLBACK_CONTACTS) {
                Ok((mut stim, notes)) => {
                    stim.label.clone_from(&label);
                    stim.subject = id;
                    warnings.extend(notes.into_iter().map(Note::plain));
                    stimulations[p] = Some(stim);
                }
                Err(e) => warnings.push(Note::plain(format!(
                    "S({}) of {} was not imported: {e}",
                    p + 1,
                    record.patients[p].subject
                ))),
            }
        }
    }
    let vatmodel = field(m, "vatmodel")
        .and_then(text)
        .filter(|s| !s.is_empty());
    let model = match vatmodel {
        Some(name) => {
            let kind = ModelKind::from_display_name(name);
            if kind.is_none() {
                warnings.push(Note::plain(format!(
                    "vatmodel {name:?} is not a known VTA model; kept as is"
                )));
                if let Some(v) = field(m, "vatmodel") {
                    pass(&mut passthrough, &dropped, "vatmodel", "M.vatmodel", v);
                }
            }
            kind
        }
        None => stimulations.iter().flatten().next().map(|s| s.model),
    };
    record.stimulation = Some(StimulationRef {
        label: group_label(&guid),
        model,
        settings_ref: "per_subject".to_owned(),
    });

    if let Some(names) = field(m, "isomatrix_name").and_then(cell_texts) {
        record.view.colour_by = names;
    }

    let top: Vec<String> = field_names(m).to_vec();
    for name in &top {
        if !KNOWN_TOP.contains(&name.as_str()) {
            if let Some(v) = field(m, name) {
                pass(&mut passthrough, &dropped, name, &format!("M.{name}"), v);
            }
        }
    }

    record.lead_group.mat_file = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    record.lead_group.passthrough = passthrough;
    record.lead_group.dropped_fields = dropped;
    Ok(MRead {
        record,
        stimulations,
        warnings,
    })
}

fn read_groups(
    m: &Mat5Value,
    record: &mut GroupRecord,
    dropped: &[String],
    passthrough: &mut Map<String, Value>,
) {
    let mut numbers_in_use: Vec<u32> = record.patients.iter().map(|p| p.group).collect();
    let groups = field(m, "groups").filter(|g| matches!(g, Mat5Value::Struct { .. }));
    let mut colors: Vec<(u32, [f64; 3])> = Vec::new();
    if let Some(g) = groups {
        let nums: Vec<u32> = field(g, "group")
            .map(numbers)
            .unwrap_or_default()
            .into_iter()
            .map(to_u32)
            .collect();
        let rows = field(g, "color")
            .map(super::matio::matrix_rows)
            .unwrap_or_default();
        if rows.len() == nums.len() {
            for (number, row) in nums.iter().zip(&rows) {
                if let [r, gr, b] = row.as_slice() {
                    colors.push((*number, [*r, *gr, *b]));
                }
            }
        }
        numbers_in_use.extend(nums);
        for name in field_names(g) {
            if !matches!(name.as_str(), "group" | "color" | "colorschosen") {
                if let Some(v) = field(g, name) {
                    pass(
                        passthrough,
                        dropped,
                        &format!("groups.{name}"),
                        &format!("M.groups.{name}"),
                        v,
                    );
                }
            }
        }
    }
    numbers_in_use.retain(|n| *n > 0);
    numbers_in_use.sort_unstable();
    numbers_in_use.dedup();
    record.groups = numbers_in_use
        .into_iter()
        .map(|number| GroupDef {
            number,
            label: format!("Group {number}"),
            color: colors.iter().find(|(n, _)| *n == number).map(|(_, c)| *c),
        })
        .collect();
}

fn read_clinical(
    m: &Mat5Value,
    record: &mut GroupRecord,
    dropped: &[String],
    passthrough: &mut Map<String, Value>,
    warnings: &mut Vec<Note>,
) {
    let Some(c) = field(m, "clinical").filter(|c| matches!(c, Mat5Value::Struct { .. })) else {
        return;
    };
    let n = record.patients.len();
    let vars: Vec<&Mat5Value> = match field(c, "vars") {
        Some(Mat5Value::Cell { items, .. }) => items.iter().collect(),
        _ => Vec::new(),
    };
    let labels = field(c, "labels").and_then(cell_texts).unwrap_or_default();
    for (i, v) in vars.iter().enumerate() {
        let label = labels
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("variable{}", i + 1));
        let dims = v.dims();
        let (rows, width) = match dims.as_slice() {
            [r, w] => (*r, *w),
            _ => (0, 0),
        };
        let data = numbers(v);
        if rows == n && width >= 1 && data.len() == rows * width {
            record
                .clinical
                .push(ClinicalVar::from_matrix(&label, rows, width, &data));
        } else {
            warnings.push(Note::coded(
                "W_LG_CLINICAL_ROWS",
                format!(
                    "clinical variable {label:?} has {rows} rows for {n} patients; it is kept but not offered for analysis"
                ),
            ));
            if !has_dropped(dropped, &format!("M.clinical.vars{{{}}}", i + 1)) {
                let mut entry = Map::new();
                entry.insert("label".to_owned(), Value::String(label));
                entry.insert("matrix".to_owned(), to_json(v));
                passthrough.insert(format!("clinical.vars[{i}]"), Value::Object(entry));
            }
        }
    }
    for name in field_names(c) {
        if !matches!(name.as_str(), "vars" | "labels") {
            if let Some(v) = field(c, name) {
                pass(
                    passthrough,
                    dropped,
                    &format!("clinical.{name}"),
                    &format!("M.clinical.{name}"),
                    v,
                );
            }
        }
    }
}

fn read_ui(
    m: &Mat5Value,
    record: &mut GroupRecord,
    env: &ReadEnv,
    dropped: &[String],
    passthrough: &mut Map<String, Value>,
    warnings: &mut Vec<Note>,
) {
    let Some(ui) = field(m, "ui").filter(|u| matches!(u, Mat5Value::Struct { .. })) else {
        return;
    };
    let d = ViewState::default();
    let view = ViewState {
        list_select: indices(field(ui, "listselect"), &d.list_select),
        clinical_list: indices(field(ui, "clinicallist"), &d.clinical_list),
        highlight_active: flag(field(ui, "hlactivecontcheck"), d.highlight_active),
        show_passive: flag(field(ui, "showpassivecontcheck"), d.show_passive),
        show_active: flag(field(ui, "showactivecontcheck"), d.show_active),
        show_isovolume: flag(field(ui, "showisovolumecheck"), d.show_isovolume),
        isovolume_display: popup(field(ui, "isovscloudpopup"), d.isovolume_display),
        elmodel_select: popup(field(ui, "elmodelselect"), d.elmodel_select),
        normalisation: popup(field(ui, "normregpopup"), d.normalisation),
        colour_contacts: flag(field(ui, "colorpointcloudcheck"), d.colour_contacts),
        mirror_sides: flag(field(ui, "mirrorsides"), d.mirror_sides),
        colour_by: Vec::new(),
        parcellation: resolve_named(field(ui, "labelpopup"), env, "parcellation", warnings),
        connectome: resolve_named(field(ui, "connectomename"), env, "connectome", warnings),
    };
    record.atlas_set = resolve_named(field(ui, "atlassetpopup"), env, "atlas set", warnings);
    record.view = view;
    for name in field_names(ui) {
        if !KNOWN_UI.contains(&name.as_str()) {
            if let Some(v) = field(ui, name) {
                pass(
                    passthrough,
                    dropped,
                    &format!("ui.{name}"),
                    &format!("M.ui.{name}"),
                    v,
                );
            }
        }
    }
}

/// One patient's inputs of the twin: the reconstruction, stimulation settings and statistics.
#[derive(Debug, Clone, Default)]
pub struct SubjectData {
    pub subject: String,
    pub reco: Option<Value>,
    pub stimulation: Option<Stimulation>,
    pub stats: Option<EaStats>,
}

/// Loads the twin inputs of every patient from the derivative folders; problems are notes.
pub fn load_subject_data(record: &GroupRecord) -> (Vec<SubjectData>, Vec<Note>) {
    let label = record.stim_label();
    let mut notes = Vec::new();
    let data = record
        .patients
        .iter()
        .map(|p| {
            let mut d = SubjectData {
                subject: p.subject.clone(),
                ..SubjectData::default()
            };
            let Some(dir) = record.patient_folder(p) else {
                return d;
            };
            let reco_path = dir
                .join("reconstruction")
                .join(format!("{}_desc-reconstruction.json", p.subject));
            if let Ok(text) = std::fs::read_to_string(&reco_path) {
                match serde_json::from_str::<Value>(&text) {
                    Ok(mut v) => {
                        for c in
                            dbsloc_reconstruct::tail_fiducial::convert_legacy_tail(&mut v, None)
                        {
                            notes.push(Note::plain(format!(
                                "{}: {}",
                                reco_path.display(),
                                c.warning
                            )));
                        }
                        d.reco = Some(v);
                    }
                    Err(e) => notes.push(Note::plain(format!(
                        "{}: unreadable reconstruction: {e}",
                        reco_path.display()
                    ))),
                }
            }
            if let Ok(stim) = load_stimulation(&dir, subject_id(&p.subject), &label) {
                d.stimulation = Some(stim);
            }
            let stats = stats_path(&dir, &p.subject);
            if stats.is_file() {
                match read_ea_stats(&stats) {
                    Ok(read) => d.stats = Some(read.stats),
                    Err(e) => notes.push(Note::coded(
                        "W_STATS_MAT_SKIPPED",
                        format!("{}: {e}", stats.display()),
                    )),
                }
            }
            d
        })
        .collect();
    (data, notes)
}

/// Options of the twin writer.
#[derive(Debug, Clone)]
pub struct TwinOptions {
    pub limit_bytes: u64,
}

impl Default for TwinOptions {
    fn default() -> Self {
        TwinOptions {
            limit_bytes: V7_LIMIT_BYTES,
        }
    }
}

/// What a twin write did.
#[derive(Debug, Clone, PartialEq)]
pub struct TwinReport {
    pub path: PathBuf,
    pub sha256: String,
    pub warnings: Vec<Note>,
}

fn rank_map(record: &GroupRecord) -> Vec<u32> {
    let mut numbers: Vec<u32> = record.groups.iter().map(|g| g.number).collect();
    numbers.extend(record.patients.iter().map(|p| p.group));
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

fn rank_of(numbers: &[u32], n: u32) -> f64 {
    numbers
        .iter()
        .position(|x| *x == n)
        .map_or(1.0, |i| (i + 1) as f64)
}

fn effective_color(record: &GroupRecord, number: u32, rank: u32) -> [f64; 3] {
    record
        .groups
        .iter()
        .find(|g| g.number == number)
        .and_then(|g| g.color)
        .unwrap_or_else(|| default_color(rank))
}

fn row_u32(values: &[u32]) -> Mat5Value {
    if values.is_empty() {
        Mat5Value::Empty
    } else {
        Mat5Value::row(&values.iter().map(|v| f64::from(*v)).collect::<Vec<_>>())
    }
}

fn flag_value(b: bool) -> Mat5Value {
    Mat5Value::scalar(f64::from(u8::from(b)))
}

fn extras(record: &GroupRecord, prefix: &str) -> Vec<(String, Mat5Value)> {
    record
        .lead_group
        .passthrough
        .iter()
        .filter_map(|(k, v)| {
            let name = if prefix.is_empty() {
                (!k.contains('.')).then_some(k.as_str())
            } else {
                k.strip_prefix(prefix)
                    .and_then(|r| r.strip_prefix('.'))
                    .filter(|r| !r.contains(['.', '[']))
            }?;
            from_json(v).map(|m| (name.to_owned(), m))
        })
        .collect()
}

fn struct_of(mut fields: Vec<(String, Mat5Value)>, extra: Vec<(String, Mat5Value)>) -> Mat5Value {
    for (name, v) in extra {
        if fields.iter().all(|(n, _)| *n != name) {
            fields.push((name, v));
        }
    }
    Mat5Value::struct_scalar(fields)
}

fn named(name: &str, v: Mat5Value) -> (String, Mat5Value) {
    (name.to_owned(), v)
}

fn clinical_field(record: &GroupRecord) -> Mat5Value {
    let mut vars: Vec<Mat5Value> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for v in &record.clinical {
        let (rows, w, data) = v.to_matrix();
        vars.push(Mat5Value::Double {
            dims: vec![rows, w],
            data,
        });
        labels.push(v.name.clone());
    }
    for (key, entry) in &record.lead_group.passthrough {
        if key.starts_with("clinical.vars[") {
            let label = entry.get("label").and_then(Value::as_str);
            let matrix = entry.get("matrix").and_then(from_json);
            if let (Some(label), Some(matrix)) = (label, matrix) {
                vars.push(matrix);
                labels.push(label.to_owned());
            }
        }
    }
    let c = vars.len();
    struct_of(
        vec![
            named(
                "vars",
                Mat5Value::Cell {
                    dims: vec![1, c],
                    items: vars,
                },
            ),
            named("labels", text_cell(vec![1, c], &labels)),
        ],
        extras(record, "clinical"),
    )
}

fn ui_field(record: &GroupRecord) -> Mat5Value {
    let v = &record.view;
    let mut fields = vec![
        named("listselect", row_u32(&v.list_select)),
        named("clinicallist", row_u32(&v.clinical_list)),
        named("hlactivecontcheck", flag_value(v.highlight_active)),
        named("showpassivecontcheck", flag_value(v.show_passive)),
        named("showactivecontcheck", flag_value(v.show_active)),
        named("showisovolumecheck", flag_value(v.show_isovolume)),
        named(
            "isovscloudpopup",
            Mat5Value::scalar(f64::from(v.isovolume_display)),
        ),
        named(
            "elmodelselect",
            Mat5Value::scalar(f64::from(v.elmodel_select)),
        ),
        named(
            "normregpopup",
            Mat5Value::scalar(f64::from(v.normalisation)),
        ),
        named("colorpointcloudcheck", flag_value(v.colour_contacts)),
        named("mirrorsides", flag_value(v.mirror_sides)),
        named(
            "atlassetpopup",
            unresolved_to_mat(record.atlas_set.as_deref().unwrap_or(DEFAULT_ATLAS_SET)),
        ),
    ];
    if let Some(p) = &v.parcellation {
        fields.push(named("labelpopup", unresolved_to_mat(p)));
    }
    fields.push(named(
        "connectomename",
        unresolved_to_mat(v.connectome.as_deref().unwrap_or(DEFAULT_CONNECTOME)),
    ));
    struct_of(fields, extras(record, "ui"))
}

fn union_struct_array(elements: Vec<Mat5Value>) -> Mat5Value {
    let mut names: Vec<String> = Vec::new();
    for e in &elements {
        for f in field_names(e) {
            if !names.contains(f) {
                names.push(f.clone());
            }
        }
    }
    let rows = elements
        .iter()
        .map(|e| {
            names
                .iter()
                .map(|n| field(e, n).cloned().unwrap_or(Mat5Value::Empty))
                .collect()
        })
        .collect::<Vec<Vec<Mat5Value>>>();
    Mat5Value::Struct {
        dims: vec![1, rows.len()],
        fields: names,
        elements: rows,
    }
}

fn pad_pair(mut items: Vec<Mat5Value>) -> Mat5Value {
    items.resize(2, Mat5Value::Empty);
    Mat5Value::cell_row(items)
}

fn elstruct_element(
    record: &GroupRecord,
    p: &PatientEntry,
    data: Option<&SubjectData>,
    ranks: &[u32],
    warnings: &mut Vec<Note>,
) -> Vec<Mat5Value> {
    let mut coords = vec![Mat5Value::Empty; 2];
    let mut traj = vec![Mat5Value::Empty; 2];
    let mut markers: Vec<Vec<Mat5Value>> = vec![vec![Mat5Value::Empty; 4]; 2];
    let mut elmodel = String::new();
    if let Some(reco) = data.and_then(|d| d.reco.as_ref()) {
        match reco_mat_from_json(reco, "", &utc_now_rfc3339()) {
            Ok(out) => {
                let reco_var = out.variables.first().map(|(_, v)| v);
                let mni = reco_var.and_then(|r| field(r, "mni"));
                if let Some(mni) = mni {
                    for (slot, name) in [(&mut coords, "coords_mm"), (&mut traj, "trajectory")] {
                        if let Some(Mat5Value::Cell { items, .. }) = field(mni, name) {
                            for (i, item) in items.iter().enumerate().take(2) {
                                slot[i] = item.clone();
                            }
                        }
                    }
                    if let Some(Mat5Value::Struct { elements, .. }) = field(mni, "markers") {
                        for (i, el) in elements.iter().enumerate().take(2) {
                            markers[i] = el.clone();
                        }
                    }
                }
                if let Some(Mat5Value::Struct {
                    fields, elements, ..
                }) = reco_var.and_then(|r| field(r, "props"))
                {
                    if let Some(at) = fields.iter().position(|f| f == "elmodel") {
                        elmodel = elements
                            .iter()
                            .filter_map(|el| el.get(at).and_then(text))
                            .find(|s| !s.is_empty())
                            .unwrap_or_default()
                            .to_owned();
                    }
                }
            }
            Err(e) => warnings.push(Note::plain(format!(
                "{}: elstruct has empty sides: {e}",
                p.subject
            ))),
        }
    }
    if let Some(over) = &p.elmodel_override {
        elmodel.clone_from(over);
    }
    if elmodel.is_empty() {
        elmodel = DEFAULT_ELMODEL.to_owned();
    }
    let group_numbers: Vec<f64> = (1..=ranks.len()).map(|i| i as f64).collect();
    let colors: Vec<Vec<f64>> = ranks
        .iter()
        .enumerate()
        .map(|(i, n)| effective_color(record, *n, (i + 1) as u32).to_vec())
        .collect();
    let marker_fields = ["head", "tail", "x", "y"];
    vec![
        Mat5Value::char(&p.subject),
        Mat5Value::char(&elmodel),
        pad_pair(coords),
        pad_pair(traj),
        Mat5Value::Struct {
            dims: vec![1, 2],
            fields: marker_fields.iter().map(|s| (*s).to_owned()).collect(),
            elements: markers,
        },
        Mat5Value::scalar(f64::NAN),
        Mat5Value::scalar(rank_of(ranks, p.group)),
        super::matio::rows_to_mat(&colors),
        column(&group_numbers),
    ]
}

/// Builds the variable `M` of the twin (`specs/14` §2.6 order) and the notes of the build.
pub fn build_m(
    record: &GroupRecord,
    subjects: &[SubjectData],
    root: &str,
    options: &TwinOptions,
) -> (Mat5Value, Vec<Note>) {
    let mut warnings: Vec<Note> = Vec::new();
    let n = record.patients.len();
    let ranks = rank_map(record);
    let label = record.stim_label();
    let data_of = |p: &PatientEntry| subjects.iter().find(|d| d.subject == p.subject);

    let list: Vec<String> = record
        .patients
        .iter()
        .map(|p| {
            record
                .patient_folder(p)
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.subject.clone())
        })
        .collect();
    let group_col: Vec<f64> = record
        .patients
        .iter()
        .map(|p| rank_of(&ranks, p.group))
        .collect();
    let patient = struct_of(
        vec![
            named("list", text_cell(vec![n, 1], &list)),
            named("group", column(&group_col)),
        ],
        extras(record, "patient"),
    );

    let mut fields: Vec<(String, Mat5Value)> = vec![
        named("patient", patient),
        named("guid", Mat5Value::char(&record.guid)),
        named("clinical", clinical_field(record)),
    ];

    let stats_for = |p: &PatientEntry| -> Option<EaStats> {
        let mut stats = data_of(p)?.stats.clone()?;
        stats.stimulation.retain(|s| s.label == label);
        Some(stats)
    };
    let all_stats: Vec<Option<EaStats>> = record.patients.iter().map(stats_for).collect();
    let first_stats = all_stats.iter().flatten().next();
    if let Some(s) = first_stats {
        fields.push(named(
            "vilist",
            text_cell(vec![1, s.atlas_names.len()], &s.atlas_names),
        ));
        let parcels = s
            .stimulation
            .iter()
            .flat_map(|st| st.ft.iter().flatten())
            .map(|f| f.labels.clone())
            .find(|l| !l.is_empty());
        if let Some(l) = parcels {
            fields.push(named("fclist", text_cell(vec![1, l.len()], &l)));
        }
    }
    fields.push(named("ui", ui_field(record)));

    let stims: Vec<Option<Stimulation>> = record
        .patients
        .iter()
        .map(|p| {
            data_of(p).and_then(|d| d.stimulation.clone()).map(|mut s| {
                s.label.clone_from(&label);
                s
            })
        })
        .collect();
    if n > 0 && stims.iter().all(Option::is_some) {
        let elements: Vec<Mat5Value> = stims.iter().flatten().map(to_lead_dbs_s).collect();
        fields.push(named("S", union_struct_array(elements)));
    } else if stims.iter().any(Option::is_some) {
        warnings.push(Note::plain(
            "S is not written: not every patient has stimulation settings under the group label"
                .to_owned(),
        ));
    }
    let model = record
        .stimulation
        .as_ref()
        .and_then(|s| s.model)
        .or_else(|| stims.iter().flatten().next().map(|s| s.model));
    fields.push(named(
        "vatmodel",
        model.map_or(Mat5Value::Empty, |m| Mat5Value::char(m.display_name())),
    ));
    fields.push(named("root", Mat5Value::char(root)));

    let mut group_numbers: Vec<f64> = Vec::new();
    let mut color_rows: Vec<Vec<f64>> = Vec::new();
    let mut explicit = false;
    for (i, number) in ranks.iter().enumerate() {
        group_numbers.push((i + 1) as f64);
        color_rows.push(effective_color(record, *number, (i + 1) as u32).to_vec());
        explicit |= record
            .groups
            .iter()
            .any(|g| g.number == *number && g.color.is_some());
    }
    let mut group_fields = vec![
        named("group", column(&group_numbers)),
        named("color", super::matio::rows_to_mat(&color_rows)),
    ];
    if explicit {
        group_fields.push(named("colorschosen", Mat5Value::scalar(1.0)));
    }
    fields.push(named(
        "groups",
        struct_of(group_fields, extras(record, "groups")),
    ));

    let elements: Vec<Vec<Mat5Value>> = record
        .patients
        .iter()
        .map(|p| elstruct_element(record, p, data_of(p), &ranks, &mut warnings))
        .collect();
    fields.push(named(
        "elstruct",
        Mat5Value::Struct {
            dims: vec![1, n],
            fields: [
                "name",
                "elmodel",
                "coords_mm",
                "trajectory",
                "markers",
                "coords_acpc",
                "group",
                "groupcolors",
                "groups",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
            elements,
        },
    ));

    let selected: Vec<&ClinicalVar> = record
        .view
        .colour_by
        .iter()
        .filter_map(|name| record.clinical.iter().find(|v| v.name == *name))
        .collect();
    let iso: Vec<Mat5Value> = selected
        .iter()
        .map(|v| {
            let (rows, w, data) = v.to_matrix();
            Mat5Value::Double {
                dims: vec![rows, w],
                data,
            }
        })
        .collect();
    let iso_names: Vec<String> = selected.iter().map(|v| v.name.clone()).collect();
    fields.push(named(
        "isomatrix",
        Mat5Value::Cell {
            dims: vec![1, iso.len()],
            items: iso,
        },
    ));
    fields.push(named(
        "isomatrix_name",
        text_cell(vec![1, iso_names.len()], &iso_names),
    ));

    let stats_value = {
        let elements: Vec<Vec<Mat5Value>> = all_stats
            .iter()
            .map(|s| vec![s.as_ref().map_or(Mat5Value::Empty, EaStats::to_mat)])
            .collect();
        Mat5Value::Struct {
            dims: vec![1, n],
            fields: vec!["ea_stats".to_owned()],
            elements,
        }
    };
    let without_stats: u64 = fields.iter().map(|(_, v)| approx_bytes(v)).sum();
    if all_stats.iter().any(Option::is_some) {
        if without_stats.saturating_add(approx_bytes(&stats_value)) > options.limit_bytes {
            warnings.push(Note::coded(
                "W_LG_STATS_OMITTED",
                "M.stats would exceed the 2 GB limit of a v7 variable and is omitted; Lead Group rebuilds it from the subject files",
            ));
        } else {
            fields.push(named("stats", stats_value));
        }
    }

    let known_extra = extras(record, "");
    let mut m = struct_of(fields, known_extra);
    if let Mat5Value::Struct { fields: f, .. } = &m {
        if f.is_empty() {
            m = Mat5Value::Empty;
        }
    }
    (m, warnings)
}

fn used_dataset_count(record: &GroupRecord) -> usize {
    let mut ids: Vec<&str> = record.patients.iter().map(|p| p.dataset.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

/// Copies a twin that this software did not write (its hash differs from the recorded one) to a
/// `_desc-backup<UTC>.mat` file; `None` when there is nothing to back up.
pub fn backup_foreign_twin(
    folder: &Path,
    record: &GroupRecord,
) -> Result<Option<PathBuf>, GroupError> {
    backup_foreign_twin_at(folder, record, &utc_digits_now())
}

/// [`backup_foreign_twin`] with an explicit `yyyymmddHHMMSS` stamp.
pub fn backup_foreign_twin_at(
    folder: &Path,
    record: &GroupRecord,
    utc_digits: &str,
) -> Result<Option<PathBuf>, GroupError> {
    let path = twin_path(folder, &record.dataset_name, &record.guid);
    if !path.is_file() {
        return Ok(None);
    }
    let sha = file_sha256_hex(&path)
        .map_err(|e| GroupError::Io(format!("cannot read {}: {e}", path.display())))?;
    if sha == record.lead_group.mat_sha256 {
        return Ok(None);
    }
    let backup = backup_path(folder, &record.dataset_name, &record.guid, utc_digits);
    std::fs::copy(&path, &backup)
        .map_err(|e| GroupError::Io(format!("cannot write {}: {e}", backup.display())))?;
    Ok(Some(backup))
}

/// Writes the Lead Group twin of `record` into `folder` (MAT v7) and records its hash. A twin
/// this software did not write is backed up first; call [`import_twin_edits`] before to keep the
/// edits made in Lead Group.
pub fn write_m_twin(
    record: &mut GroupRecord,
    subjects: &[SubjectData],
    folder: &Path,
) -> Result<TwinReport, GroupError> {
    write_m_twin_with(record, subjects, folder, &TwinOptions::default())
}

/// [`write_m_twin`] with explicit options.
pub fn write_m_twin_with(
    record: &mut GroupRecord,
    subjects: &[SubjectData],
    folder: &Path,
    options: &TwinOptions,
) -> Result<TwinReport, GroupError> {
    let mut warnings = Vec::new();
    if !is_alphanumeric_id(&record.dataset_name) || !is_alphanumeric_id(&record.guid) {
        warnings.push(Note::coded(
            "W_LG_NAME",
            format!(
                "the dataset name {:?} or the guid {:?} holds characters other than letters and digits; Lead Group will not find the file",
                record.dataset_name, record.guid
            ),
        ));
    }
    if used_dataset_count(record) > 1 {
        warnings.push(Note::coded(
            "W_LG_MULTI_DATASET",
            "the patients come from several datasets; Lead Group remaps the folders of the first patient only",
        ));
    }
    if let Some(b) = backup_foreign_twin(folder, record)? {
        warnings.push(Note::plain(format!(
            "the existing Lead Group file was not written by this software; copied to {}",
            b.display()
        )));
    }
    let root = format!(
        "{}{}",
        folder.to_string_lossy().trim_end_matches(['/', '\\']),
        std::path::MAIN_SEPARATOR
    );
    let (m, mut build_notes) = build_m(record, subjects, &root, options);
    warnings.append(&mut build_notes);
    let bytes = mat5_bytes(&[(M_VARIABLE.to_owned(), m)], true)
        .map_err(|e| GroupError::Mat(e.to_string()))?;
    let path = twin_path(folder, &record.dataset_name, &record.guid);
    write_replacing(&path, &bytes)
        .map_err(|e| GroupError::Io(format!("cannot write {}: {e}", path.display())))?;
    let sha = sha256_hex(&bytes);
    record.lead_group.mat_file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    record.lead_group.mat_sha256.clone_from(&sha);
    Ok(TwinReport {
        path,
        sha256: sha,
        warnings,
    })
}

/// What importing the edits of a twin did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TwinImport {
    pub changed: Vec<String>,
    pub stimulations: Vec<Option<Stimulation>>,
    pub warnings: Vec<Note>,
}

fn same_values(a: &[ClinicalVar], b: &[ClinicalVar]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.name == y.name
                && x.values.len() == y.values.len()
                && x.values.iter().zip(&y.values).all(|(r, s)| r == s)
        })
}

fn close(a: &[f64; 3], b: &[f64; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
}

/// Imports the edits made outside this software into `record` when the twin in `folder` differs
/// from the recorded hash (`specs/14` §2.7). The twin wins for patients, groups, colours, clinical
/// variables, `S` and the view; the recorded hash is updated.
pub fn import_twin_edits(
    record: &mut GroupRecord,
    folder: &Path,
) -> Result<TwinImport, GroupError> {
    let path = twin_path(folder, &record.dataset_name, &record.guid);
    let mut out = TwinImport::default();
    if !path.is_file() {
        return Ok(out);
    }
    let sha = file_sha256_hex(&path)
        .map_err(|e| GroupError::Io(format!("cannot read {}: {e}", path.display())))?;
    if sha == record.lead_group.mat_sha256 {
        return Ok(out);
    }
    let read = read_m(&path)?;
    let mut new = read.record;
    out.warnings = read.warnings;

    let old_numbers = rank_map(record);
    let to_old = |r: u32| -> u32 {
        old_numbers
            .get((r as usize).wrapping_sub(1))
            .copied()
            .unwrap_or(r)
    };
    for p in &mut new.patients {
        p.group = to_old(p.group);
        if let Some(old) = record.patients.iter().find(|o| o.subject == p.subject) {
            p.elmodel_override.clone_from(&old.elmodel_override);
            p.included = old.included;
        }
    }
    for g in &mut new.groups {
        g.number = to_old(g.number);
        if let Some(old) = record.groups.iter().find(|o| o.number == g.number) {
            g.label.clone_from(&old.label);
        }
    }
    new.groups.sort_by_key(|g| g.number);
    let old_ranks = rank_map(record);
    for g in &mut new.groups {
        let rank = rank_of(&old_ranks, g.number) as u32;
        let old_effective = effective_color(record, g.number, rank);
        if let Some(c) = g.color {
            if close(&c, &old_effective) {
                g.color = record
                    .groups
                    .iter()
                    .find(|o| o.number == g.number)
                    .and_then(|o| o.color);
            }
        }
    }
    for v in &mut new.clinical {
        if let Some(old) = record.clinical.iter().find(|o| o.name == v.name) {
            if old.width() == v.width() {
                v.kind = old.kind;
            }
            v.measure.clone_from(&old.measure);
            v.higher_is_better = old.higher_is_better;
            v.unit.clone_from(&old.unit);
            v.description.clone_from(&old.description);
            v.source.clone_from(&old.source);
        }
    }

    let subjects_same = record.patients.len() == new.patients.len()
        && record
            .patients
            .iter()
            .zip(&new.patients)
            .all(|(a, b)| a.subject == b.subject);
    if !subjects_same {
        out.changed.push("patients".to_owned());
    }
    let groups_same = subjects_same
        && record
            .patients
            .iter()
            .zip(&new.patients)
            .all(|(a, b)| a.group == b.group)
        && record.groups.len() == new.groups.len()
        && record.groups.iter().zip(&new.groups).all(|(a, b)| {
            a.number == b.number
                && match (a.color, b.color) {
                    (None, None) => true,
                    (Some(x), Some(y)) => close(&x, &y),
                    _ => false,
                }
        });
    if !groups_same {
        out.changed.push("groups".to_owned());
    }
    if !same_values(&record.clinical, &new.clinical) {
        out.changed.push("clinical".to_owned());
    }
    if record.view != new.view {
        out.changed.push("ui".to_owned());
    }
    if record.atlas_set != new.atlas_set {
        out.changed.push("atlas_set".to_owned());
    }
    let new_model = new.stimulation.as_ref().and_then(|s| s.model);
    if record.stimulation.as_ref().and_then(|s| s.model) != new_model && new_model.is_some() {
        out.changed.push("vatmodel".to_owned());
    }

    let mut next = new;
    next.datasets = {
        let mut datasets = std::mem::take(&mut next.datasets);
        for d in &mut datasets {
            if let Some(old) = record
                .datasets
                .iter()
                .find(|o| o.derivatives_root == d.derivatives_root)
            {
                let new_id = old.id.clone();
                for p in &mut next.patients {
                    if p.dataset == d.id {
                        p.dataset.clone_from(&new_id);
                    }
                }
                d.id = new_id;
            }
        }
        datasets
    };
    let label = record.stim_label();
    let any_s = read.stimulations.iter().any(Option::is_some);
    if any_s {
        let differs = next.patients.iter().zip(&read.stimulations).any(|(p, s)| {
            let Some(s) = s else { return false };
            let on_disk = next
                .patient_folder(p)
                .and_then(|dir| load_stimulation(&dir, subject_id(&p.subject), &label).ok());
            match on_disk {
                Some(d) => {
                    d.sides != s.sides || d.model_settings != s.model_settings || d.model != s.model
                }
                None => true,
            }
        });
        if differs {
            out.changed.push("S".to_owned());
        }
    }
    out.stimulations = read.stimulations;

    record.datasets = next.datasets;
    record.patients = next.patients;
    record.groups = next.groups;
    record.clinical = next.clinical;
    record.view = next.view;
    record.atlas_set = next.atlas_set;
    if let (Some(dst), Some(src)) = (record.stimulation.as_mut(), next.stimulation) {
        if src.model.is_some() {
            dst.model = src.model;
        }
    }
    record.lead_group.passthrough = next.lead_group.passthrough;
    record.lead_group.dropped_fields = next.lead_group.dropped_fields;
    record.lead_group.mat_sha256 = sha;
    record.lead_group.mat_file = next.lead_group.mat_file;
    Ok(out)
}

/// Saves the imported stimulation settings of patients whose settings differ from the files on
/// disk; returns the subjects written.
pub fn apply_imported_stimulations(
    record: &GroupRecord,
    stimulations: &[Option<Stimulation>],
) -> Result<Vec<String>, GroupError> {
    let label = record.stim_label();
    let mut written = Vec::new();
    for (p, stim) in record.patients.iter().zip(stimulations) {
        let (Some(stim), Some(dir)) = (stim, record.patient_folder(p)) else {
            continue;
        };
        let current = load_stimulation(&dir, subject_id(&p.subject), &label).ok();
        let same = current.as_ref().is_some_and(|c| {
            c.sides == stim.sides
                && c.model_settings == stim.model_settings
                && c.model == stim.model
        });
        if same {
            continue;
        }
        if !dir.is_dir() {
            continue;
        }
        crate::vat::settings::save(&dir, stim).map_err(|e| GroupError::Io(e.to_string()))?;
        written.push(p.subject.clone());
    }
    Ok(written)
}
