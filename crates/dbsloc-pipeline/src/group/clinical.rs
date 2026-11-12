// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Clinical variables of a group (`specs/14` §10): definition, table import, storage, export.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::record::{file_stem, subject_id, GroupRecord};
use super::GroupError;
use crate::outputs::write_replacing;
use crate::version::{software_stamp, utc_now_rfc3339};

const MISSING_TOKENS: [&str; 4] = ["", "na", "nan", "n/a"];
const SUBJECT_COLUMNS: [&str; 3] = ["participant_id", "subject", "sub"];

fn yes() -> bool {
    true
}

fn other() -> String {
    "other".to_owned()
}

/// What the columns of a clinical variable mean (`specs/14` §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClinicalKind {
    PerPatient,
    PerHemisphere,
    PerContact,
    PerContactPair,
}

/// A clinical variable: a name and N rows of `w` values, `None` = missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClinicalVar {
    pub name: String,
    pub kind: ClinicalKind,
    pub values: Vec<Vec<Option<f64>>>,
    #[serde(default = "other")]
    pub measure: String,
    #[serde(default = "yes")]
    pub higher_is_better: bool,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: String,
}

/// Kind implied by the column count of a Lead Group variable (8 columns are per contact, 6 per pair).
pub fn kind_for_width(w: usize) -> ClinicalKind {
    match w {
        2 => ClinicalKind::PerHemisphere,
        6 => ClinicalKind::PerContactPair,
        w if w >= 4 && w % 2 == 0 => ClinicalKind::PerContact,
        _ => ClinicalKind::PerPatient,
    }
}

impl ClinicalVar {
    /// A variable of the given kind with all values missing.
    pub fn blank(name: &str, kind: ClinicalKind, rows: usize, width: usize) -> ClinicalVar {
        ClinicalVar {
            name: name.to_owned(),
            kind,
            values: vec![vec![None; width]; rows],
            measure: other(),
            higher_is_better: true,
            unit: String::new(),
            description: String::new(),
            source: String::new(),
        }
    }

    /// Number of columns.
    pub fn width(&self) -> usize {
        match self.kind {
            ClinicalKind::PerPatient => self.values.first().map_or(1, Vec::len).max(1),
            ClinicalKind::PerHemisphere => 2,
            _ => self.values.first().map_or(2, Vec::len),
        }
    }

    /// From a column-major `rows × width` matrix; NaN becomes missing.
    pub fn from_matrix(name: &str, rows: usize, width: usize, data: &[f64]) -> ClinicalVar {
        let mut var = ClinicalVar::blank(name, kind_for_width(width), rows, width);
        for (i, row) in var.values.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = data.get(j * rows + i).copied().filter(|v| !v.is_nan());
            }
        }
        var.source = "lead_group".to_owned();
        var
    }

    /// Column-major `rows × width` matrix with NaN for missing values.
    pub fn to_matrix(&self) -> (usize, usize, Vec<f64>) {
        let rows = self.values.len();
        let w = self.width();
        let mut data = vec![f64::NAN; rows * w];
        for (i, row) in self.values.iter().enumerate() {
            for (j, v) in row.iter().enumerate().take(w) {
                if let Some(v) = v {
                    data[j * rows + i] = *v;
                }
            }
        }
        (rows, w, data)
    }

    /// Column headers of the TSV export.
    pub fn columns(&self) -> Vec<String> {
        let name = sanitize(&self.name);
        let w = self.width();
        match self.kind {
            ClinicalKind::PerPatient if w == 1 => vec![name],
            ClinicalKind::PerPatient => (1..=w).map(|i| format!("{name}_{i}")).collect(),
            ClinicalKind::PerHemisphere => vec![format!("{name}_R"), format!("{name}_L")],
            ClinicalKind::PerContact => {
                let k = w / 2;
                ["R", "L"]
                    .iter()
                    .flat_map(|s| (1..=k).map(move |n| (n, *s)))
                    .map(|(n, s)| format!("{name}_k{n}{s}"))
                    .collect()
            }
            ClinicalKind::PerContactPair => {
                let k = w / 2;
                ["R", "L"]
                    .iter()
                    .flat_map(|s| (1..=k).map(move |n| (n, *s)))
                    .map(|(n, s)| format!("{name}_k{n}-{}{s}", n + 1))
                    .collect()
            }
        }
    }
}

fn sanitize(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Options of a table import.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub replace: bool,
    pub subject_column: Option<String>,
    pub source: String,
}

/// What a table import did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ImportReport {
    pub imported: Vec<String>,
    pub replaced: Vec<String>,
    pub unknown_subjects: Vec<String>,
    pub patients_without_row: Vec<String>,
}

/// Errors of the clinical table import.
#[derive(Debug, thiserror::Error)]
pub enum ClinicalError {
    #[error("clinical table: {0}")]
    Table(String),
    #[error("clinical table: no row matches a patient of the group (subject column {column:?}, first value {first:?})")]
    NoSubjectMatches { column: String, first: String },
    #[error("clinical variable(s) already exist: {0}; use --replace to replace them")]
    Exists(String),
}

impl From<ClinicalError> for GroupError {
    fn from(e: ClinicalError) -> Self {
        GroupError::Clinical(e.to_string())
    }
}

/// Delimiter from the file extension: tab for `.tsv`, comma for `.csv`.
pub fn delimiter_for(path: &Path) -> Result<char, ClinicalError> {
    match path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("tsv") => Ok('\t'),
        Some("csv") => Ok(','),
        _ => Err(ClinicalError::Table(format!(
            "{}: the extension must be .tsv or .csv",
            path.display()
        ))),
    }
}

fn parse_rows(text: &str, delimiter: char) -> Result<Vec<Vec<String>>, ClinicalError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut touched = false;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && delimiter == ',' => {
                quoted = true;
                touched = true;
            }
            c if c == delimiter => {
                row.push(std::mem::take(&mut field));
                touched = true;
            }
            '\r' => {}
            '\n' => {
                if touched || !field.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                touched = false;
            }
            c => {
                field.push(c);
                touched = true;
            }
        }
    }
    if quoted {
        return Err(ClinicalError::Table("unterminated quoted field".to_owned()));
    }
    if touched || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn parse_value(raw: &str, delimiter: char) -> Result<Option<f64>, String> {
    let t = raw.trim();
    if MISSING_TOKENS.contains(&t.to_ascii_lowercase().as_str()) {
        return Ok(None);
    }
    let text = if delimiter == '\t' && t.contains(',') && !t.contains('.') {
        t.replace(',', ".")
    } else {
        t.to_owned()
    };
    match text.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(Some(v)),
        _ => Err(t.to_owned()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SideTag {
    R,
    L,
}

fn side_suffix(col: &str) -> Option<(String, SideTag, Option<usize>)> {
    let lower = col.to_ascii_lowercase();
    for (tail, side) in [(" right", SideTag::R), (" left", SideTag::L)] {
        if lower.ends_with(tail) && col.len() > tail.len() {
            return Some((col[..col.len() - tail.len()].to_owned(), side, None));
        }
    }
    let side = match col.chars().last()? {
        'R' => SideTag::R,
        'L' => SideTag::L,
        _ => return None,
    };
    let stem = &col[..col.len() - 1];
    if let Some(at) = stem.rfind("_k") {
        let digits = &stem[at + 2..];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let k = digits.parse::<usize>().ok()?;
            if at > 0 && k >= 1 {
                return Some((stem[..at].to_owned(), side, Some(k)));
            }
        }
    }
    if stem.ends_with('_') && stem.len() > 1 {
        return Some((stem[..stem.len() - 1].to_owned(), side, None));
    }
    None
}

struct Planned {
    name: String,
    kind: ClinicalKind,
    columns: Vec<usize>,
}

fn plan_variables(header: &[String], skip: usize) -> Vec<Planned> {
    let mut planned: Vec<Planned> = Vec::new();
    let mut used = vec![false; header.len()];
    let parsed: Vec<Option<(String, SideTag, Option<usize>)>> =
        header.iter().map(|h| side_suffix(h.trim())).collect();
    for i in 0..header.len() {
        if i == skip || used[i] {
            continue;
        }
        if let Some((base, _, Some(_))) = &parsed[i] {
            let mut by_side: [Vec<(usize, usize)>; 2] = [Vec::new(), Vec::new()];
            for (j, p) in parsed.iter().enumerate() {
                if let Some((b, s, Some(k))) = p {
                    if b == base && j != skip && !used[j] {
                        by_side[usize::from(*s == SideTag::L)].push((*k, j));
                    }
                }
            }
            for side in &mut by_side {
                side.sort_unstable();
            }
            let k = by_side[0].len();
            let contiguous =
                |s: &[(usize, usize)]| s.iter().enumerate().all(|(n, (k, _))| *k == n + 1);
            if k >= 1 && by_side[1].len() == k && contiguous(&by_side[0]) && contiguous(&by_side[1])
            {
                let columns: Vec<usize> = by_side[0]
                    .iter()
                    .chain(&by_side[1])
                    .map(|(_, j)| *j)
                    .collect();
                for j in &columns {
                    used[*j] = true;
                }
                planned.push(Planned {
                    name: base.clone(),
                    kind: ClinicalKind::PerContact,
                    columns,
                });
                continue;
            }
        }
        if let Some((base, SideTag::R, None)) = &parsed[i] {
            let left = parsed.iter().enumerate().find(|(j, p)| {
                !used[*j] && *j != skip && matches!(p, Some((b, SideTag::L, None)) if b == base)
            });
            if let Some((j, _)) = left {
                used[i] = true;
                used[j] = true;
                planned.push(Planned {
                    name: base.clone(),
                    kind: ClinicalKind::PerHemisphere,
                    columns: vec![i, j],
                });
                continue;
            }
        }
        if let Some((base, SideTag::L, None)) = &parsed[i] {
            let right = parsed.iter().enumerate().find(|(j, p)| {
                !used[*j] && *j != skip && matches!(p, Some((b, SideTag::R, None)) if b == base)
            });
            if let Some((j, _)) = right {
                used[i] = true;
                used[j] = true;
                planned.push(Planned {
                    name: base.clone(),
                    kind: ClinicalKind::PerHemisphere,
                    columns: vec![j, i],
                });
                continue;
            }
        }
        used[i] = true;
        planned.push(Planned {
            name: header[i].trim().to_owned(),
            kind: ClinicalKind::PerPatient,
            columns: vec![i],
        });
    }
    planned
}

fn normalise_subject(s: &str) -> String {
    subject_id(s.trim()).to_owned()
}

/// Imports a TSV / CSV table into the clinical variables of `record` (`specs/14` §10.3).
/// The record is changed only when the whole import succeeds.
pub fn import_table(
    record: &mut GroupRecord,
    text: &str,
    delimiter: char,
    options: &ImportOptions,
) -> Result<ImportReport, ClinicalError> {
    let rows = parse_rows(text, delimiter)?;
    let Some((header, body)) = rows.split_first() else {
        return Err(ClinicalError::Table("the table is empty".to_owned()));
    };
    let id_col = match &options.subject_column {
        Some(name) => header
            .iter()
            .position(|h| h.trim() == name.trim())
            .ok_or_else(|| ClinicalError::Table(format!("no column named {name:?}")))?,
        None => header
            .iter()
            .position(|h| SUBJECT_COLUMNS.contains(&h.trim().to_ascii_lowercase().as_str()))
            .unwrap_or(0),
    };
    let n = record.patients.len();
    let mut row_of: Vec<Option<usize>> = vec![None; n];
    let mut report = ImportReport::default();
    let mut first_value = String::new();
    for (r, row) in body.iter().enumerate() {
        let Some(raw) = row.get(id_col) else {
            return Err(ClinicalError::Table(format!(
                "row {} has no subject column",
                r + 2
            )));
        };
        if first_value.is_empty() {
            first_value = raw.trim().to_owned();
        }
        let wanted = normalise_subject(raw);
        match record
            .patients
            .iter()
            .position(|p| normalise_subject(&p.subject) == wanted)
        {
            Some(p) if row_of[p].is_none() => row_of[p] = Some(r),
            Some(_) => {}
            None => report.unknown_subjects.push(raw.trim().to_owned()),
        }
    }
    if n > 0 && row_of.iter().all(Option::is_none) {
        return Err(ClinicalError::NoSubjectMatches {
            column: header.get(id_col).cloned().unwrap_or_default(),
            first: first_value,
        });
    }
    report.patients_without_row = record
        .patients
        .iter()
        .zip(&row_of)
        .filter(|(_, r)| r.is_none())
        .map(|(p, _)| p.subject.clone())
        .collect();
    let mut built: Vec<ClinicalVar> = Vec::new();
    for plan in plan_variables(header, id_col) {
        let mut var = ClinicalVar::blank(&plan.name, plan.kind, n, plan.columns.len());
        var.source = format!("tsv:{}", options.source);
        for (p, r) in row_of.iter().enumerate() {
            let Some(r) = r else { continue };
            for (c, col) in plan.columns.iter().enumerate() {
                let raw = body[*r].get(*col).map_or("", String::as_str);
                var.values[p][c] = parse_value(raw, delimiter).map_err(|bad| {
                    ClinicalError::Table(format!(
                        "row {}, column {:?}: {bad:?} is not a number",
                        r + 2,
                        header.get(*col).cloned().unwrap_or_default()
                    ))
                })?;
            }
        }
        built.push(var);
    }
    let clashes: Vec<String> = built
        .iter()
        .filter(|v| record.clinical.iter().any(|e| e.name == v.name))
        .map(|v| v.name.clone())
        .collect();
    if !clashes.is_empty() && !options.replace {
        return Err(ClinicalError::Exists(clashes.join(", ")));
    }
    for var in built {
        match record.clinical.iter().position(|e| e.name == var.name) {
            Some(i) => {
                let old = &record.clinical[i];
                let mut var = var;
                if old.kind == var.kind && old.width() == var.width() {
                    var.measure = old.measure.clone();
                    var.higher_is_better = old.higher_is_better;
                    var.unit = old.unit.clone();
                    var.description = old.description.clone();
                }
                report.replaced.push(var.name.clone());
                record.clinical[i] = var;
            }
            None => {
                report.imported.push(var.name.clone());
                record.clinical.push(var);
            }
        }
    }
    Ok(report)
}

/// `<group folder>/exports/dataset-<d>_analysis-<guid>_desc-clinical.tsv`.
pub fn clinical_tsv_path(folder: &Path, dataset_name: &str, guid: &str) -> PathBuf {
    folder.join("exports").join(format!(
        "{}_desc-clinical.tsv",
        file_stem(dataset_name, guid)
    ))
}

fn number_text(v: Option<f64>) -> String {
    v.map_or_else(String::new, |v| format!("{v}"))
}

/// The clinical TSV of `specs/14` §10.5.
pub fn export_tsv(record: &GroupRecord) -> String {
    let mut header = vec![
        "participant_id".to_owned(),
        "group".to_owned(),
        "group_label".to_owned(),
    ];
    for v in &record.clinical {
        header.extend(v.columns());
    }
    let mut out = header.join("\t");
    out.push('\n');
    for (p, patient) in record.patients.iter().enumerate() {
        let mut cells = vec![
            sanitize(&patient.subject),
            patient.group.to_string(),
            sanitize(&record.group_label_of(patient.group)),
        ];
        for v in &record.clinical {
            let w = v.columns().len();
            for c in 0..w {
                cells.push(number_text(
                    v.values.get(p).and_then(|r| r.get(c)).copied().flatten(),
                ));
            }
        }
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

/// The JSON sidecar of the clinical TSV (BIDS phenotype style).
pub fn export_sidecar(record: &GroupRecord, tsv_name: &str) -> Value {
    let variables: Vec<Value> = record
        .clinical
        .iter()
        .map(|v| {
            json!({
                "name": v.name,
                "kind": v.kind,
                "columns": v.columns(),
                "measure": v.measure,
                "higher_is_better": v.higher_is_better,
                "unit": v.unit,
                "description": v.description,
                "source": v.source,
            })
        })
        .collect();
    json!({
        "software": software_stamp(),
        "guid": record.guid,
        "dataset": record.dataset_name,
        "file": tsv_name,
        "created_at": utc_now_rfc3339(),
        "variables": variables,
    })
}

/// Writes the clinical TSV and its JSON sidecar under `<group folder>/exports/`.
pub fn write_clinical_export(
    record: &GroupRecord,
    folder: &Path,
) -> Result<(PathBuf, PathBuf), GroupError> {
    let tsv = clinical_tsv_path(folder, &record.dataset_name, &record.guid);
    let sidecar = tsv.with_extension("json");
    let name = tsv
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    write_replacing(&tsv, export_tsv(record).as_bytes())
        .map_err(|e| GroupError::Io(format!("cannot write {}: {e}", tsv.display())))?;
    let text = serde_json::to_string_pretty(&export_sidecar(record, &name))
        .map(|t| t + "\n")
        .map_err(|e| GroupError::Json(e.to_string()))?;
    write_replacing(&sidecar, text.as_bytes())
        .map_err(|e| GroupError::Io(format!("cannot write {}: {e}", sidecar.display())))?;
    Ok((tsv, sidecar))
}
