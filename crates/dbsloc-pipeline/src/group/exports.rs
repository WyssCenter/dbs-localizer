// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Group statistics exports (`specs/14` §6.4, §11): TSV / CSV tables with a JSON sidecar and the
//! Lead-DBS statistics export (MAT v7).

use std::path::{Path, PathBuf};

use fastwarp_matfile::mat5::{mat5_bytes, Mat5Value};
use serde_json::{json, Value};

use super::ea_stats::{read_ea_stats, stats_path};
use super::overlap::{
    load_contact_rows, load_vta_stats, summary_rows, vta_rows, vtastats_path, ContactRow,
    SubjectVta, SummaryRow, VtaLoad, VtaRow,
};
use super::record::{file_stem, subject_id, GroupRecord};
use super::{GroupError, Note};
use crate::outputs::write_replacing;
use crate::sha256::file_sha256_hex;
use crate::vat::settings::Side;
use crate::version::{software_stamp, utc_now_rfc3339};

/// Columns of the cohort VTA table (`specs/14` §6.1).
pub const VTA_COLUMNS: [&str; 15] = [
    "subject",
    "group",
    "group_label",
    "side",
    "source",
    "structure",
    "vta_volume_mm3",
    "overlap_mm3",
    "frac_of_vta",
    "frac_of_structure",
    "structure_volume_mm3",
    "efield_sum",
    "efield_in_structure",
    "frac_of_efield",
    "amplitude",
];

/// Columns of the contact distance table (`specs/14` §6.2).
pub const CONTACT_COLUMNS: [&str; 10] = [
    "subject",
    "group",
    "group_label",
    "side",
    "contact",
    "structure",
    "distance_mm",
    "inside_hull",
    "inside_voxel",
    "active",
];

/// Columns of the summary table (`specs/14` §6.3).
pub const SUMMARY_COLUMNS: [&str; 11] = [
    "group",
    "group_label",
    "side",
    "structure",
    "measure",
    "n",
    "mean",
    "sd",
    "median",
    "min",
    "max",
];

/// One table cell; a missing number or flag is an empty cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Text(String),
    Num(Option<f64>),
    Int(u64),
    Bool(Option<bool>),
}

/// A table with a header and rows of cells.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: &'static [&'static str],
    pub rows: Vec<Vec<Cell>>,
}

fn text(s: &str) -> Cell {
    Cell::Text(s.to_owned())
}

fn num(v: Option<f64>) -> Cell {
    Cell::Num(v.filter(|x| x.is_finite()))
}

/// The cohort VTA table.
pub fn vta_table(rows: &[VtaRow]) -> Table {
    Table {
        columns: &VTA_COLUMNS,
        rows: rows
            .iter()
            .map(|r| {
                vec![
                    text(&r.subject),
                    Cell::Int(u64::from(r.group)),
                    text(&r.group_label),
                    Cell::Text(r.side.to_string()),
                    Cell::Int(u64::from(r.source)),
                    text(&r.structure),
                    num(r.vta_volume_mm3),
                    num(r.overlap_mm3),
                    num(r.frac_of_vta),
                    num(r.frac_of_structure),
                    num(r.structure_volume_mm3),
                    num(r.efield_sum),
                    num(r.efield_in_structure),
                    num(r.frac_of_efield),
                    text(&r.amplitude),
                ]
            })
            .collect(),
    }
}

/// The contact distance table.
pub fn contact_table(rows: &[ContactRow]) -> Table {
    Table {
        columns: &CONTACT_COLUMNS,
        rows: rows
            .iter()
            .map(|r| {
                vec![
                    text(&r.subject),
                    Cell::Int(u64::from(r.group)),
                    text(&r.group_label),
                    Cell::Text(r.side.to_string()),
                    Cell::Int(u64::from(r.contact)),
                    text(&r.structure),
                    num(r.distance_mm),
                    Cell::Bool(r.inside_hull),
                    Cell::Bool(r.inside_voxel),
                    Cell::Bool(r.active),
                ]
            })
            .collect(),
    }
}

/// The summary table.
pub fn summary_table(rows: &[SummaryRow]) -> Table {
    Table {
        columns: &SUMMARY_COLUMNS,
        rows: rows
            .iter()
            .map(|r| {
                vec![
                    Cell::Int(u64::from(r.group)),
                    text(&r.group_label),
                    text(r.side),
                    text(&r.structure),
                    text(r.measure),
                    Cell::Int(r.n as u64),
                    num(r.mean),
                    num(r.sd),
                    num(r.median),
                    num(r.min),
                    num(r.max),
                ]
            })
            .collect(),
    }
}

fn cell_text(c: &Cell) -> String {
    match c {
        Cell::Text(s) => s.clone(),
        Cell::Num(Some(v)) => format!("{v}"),
        Cell::Num(None) | Cell::Bool(None) => String::new(),
        Cell::Int(v) => v.to_string(),
        Cell::Bool(Some(b)) => b.to_string(),
    }
}

fn tsv_field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// Tab-separated text: header row, `\n` line ends, `.` decimals, empty cells for missing values.
pub fn to_tsv(table: &Table) -> String {
    let mut out = table.columns.join("\t");
    out.push('\n');
    for row in &table.rows {
        let cells: Vec<String> = row.iter().map(|c| tsv_field(&cell_text(c))).collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

/// Comma-separated text with RFC 4180 quoting and `\r\n` line ends.
pub fn to_csv(table: &Table) -> String {
    let mut out = table.columns.join(",");
    out.push_str("\r\n");
    for row in &table.rows {
        let cells: Vec<String> = row.iter().map(|c| csv_field(&cell_text(c))).collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

/// `<folder>/exports/dataset-<d>_analysis-<guid>_desc-<desc>_stats.<ext>`.
pub fn table_path(dir: &Path, record: &GroupRecord, desc: &str, ext: &str) -> PathBuf {
    dir.join(format!(
        "{}_desc-{desc}_stats.{ext}",
        file_stem(&record.dataset_name, &record.guid)
    ))
}

/// `<folder>/dataset-<d>_analysis-<guid>_desc-stats_export.mat`.
pub fn lead_dbs_export_path(dir: &Path, record: &GroupRecord) -> PathBuf {
    dir.join(format!(
        "{}_desc-stats_export.mat",
        file_stem(&record.dataset_name, &record.guid)
    ))
}

fn clean_cell(v: &[Option<f64>]) -> Mat5Value {
    let data: Vec<f64> = v.iter().map(|x| x.unwrap_or(f64::NAN)).collect();
    Mat5Value::row(&data)
}

fn side_cells(
    subjects: &[Option<&SubjectVta>],
    pick: impl Fn(&super::overlap::SideVta) -> Option<&Vec<Option<f64>>>,
) -> Mat5Value {
    let n = subjects.len();
    let mut items = Vec::with_capacity(2 * n);
    for side in Side::ALL {
        for s in subjects {
            let value = s
                .and_then(|s| s.sides[side.index()].as_ref())
                .and_then(&pick)
                .filter(|v| !v.is_empty());
            items.push(value.map_or(Mat5Value::Empty, |v| clean_cell(v)));
        }
    }
    Mat5Value::Cell {
        dims: vec![n, 2],
        items,
    }
}

/// The variables of the Lead-DBS statistics export (`specs/14` §6.4). Rows follow the patient order of the record; a patient without statistics has empty cells. The fibre count variables are not written (the first release computes no parcel fibre counts, `specs/14` §3.2).
pub fn lead_dbs_export_variables(
    record: &GroupRecord,
    vta: &[SubjectVta],
) -> Vec<(String, Mat5Value)> {
    let by_patient: Vec<Option<&SubjectVta>> = record
        .patients
        .iter()
        .map(|p| vta.iter().find(|s| s.subject == p.subject))
        .collect();
    let names: Vec<String> = vta
        .first()
        .map(|s| s.structures.clone())
        .unwrap_or_default();
    let clinical_names: Vec<Mat5Value> = record
        .clinical
        .iter()
        .map(|c| Mat5Value::char(&c.name))
        .collect();
    let clinical_values: Vec<Mat5Value> = record
        .clinical
        .iter()
        .map(|c| {
            let (rows, cols, data) = c.to_matrix();
            Mat5Value::Double {
                dims: vec![rows, cols],
                data,
            }
        })
        .collect();
    let c = clinical_names.len();
    let selected = record.view.clinical_list.first().copied().unwrap_or(1);
    let mut vars = vec![
        (
            "stimulation".to_owned(),
            Mat5Value::char(&record.stim_label()),
        ),
        (
            "clinicalVars".to_owned(),
            Mat5Value::Cell {
                dims: vec![c, 1],
                items: clinical_values,
            },
        ),
        (
            "clinicalVarNames".to_owned(),
            Mat5Value::Cell {
                dims: vec![c, 1],
                items: clinical_names,
            },
        ),
        (
            "clinicalVarSelected".to_owned(),
            Mat5Value::scalar(f64::from(selected)),
        ),
        (
            "atlas".to_owned(),
            Mat5Value::char(record.atlas_set.as_deref().unwrap_or("")),
        ),
        (
            "atlasNames".to_owned(),
            Mat5Value::Cell {
                dims: vec![names.len(), 1],
                items: names.iter().map(|n| Mat5Value::char(n)).collect(),
            },
        ),
    ];
    if let Some(p) = &record.view.parcellation {
        vars.push(("parcellation".to_owned(), Mat5Value::char(p)));
    }
    if let Some(p) = &record.view.connectome {
        vars.push(("connectome".to_owned(), Mat5Value::char(p)));
    }
    vars.push((
        "AtlasIntersectionVAT".to_owned(),
        side_cells(&by_patient, |s| Some(&s.intersection)),
    ));
    vars.push((
        "nAtlasIntersectionVAT".to_owned(),
        side_cells(&by_patient, |s| Some(&s.n_intersection)),
    ));
    vars.push((
        "AtlasIntersectionEfield".to_owned(),
        side_cells(&by_patient, |s| {
            s.efield_volume
                .is_some_and(|e| e != 0.0)
                .then_some(&s.efield_intersection)
        }),
    ));
    vars.push((
        "nAtlasIntersectionEfield".to_owned(),
        side_cells(&by_patient, |s| {
            s.efield_volume
                .is_some_and(|e| e != 0.0)
                .then_some(&s.efield_n_intersection)
        }),
    ));
    vars
}

type FibreCounts = (Vec<f64>, Vec<f64>);

/// `fibercounts` and `nfibercounts` of the Lead-DBS statistics export (`specs/14` §6.4): N × 2 cells, row p = patient, column = side, from the parcel fibre counts (`ft`) that a patient's statistics file holds for the group label. Empty when no patient has any, so the variables are left out.
pub fn fibre_count_variables(record: &GroupRecord) -> Vec<(String, Mat5Value)> {
    let label = record.stim_label();
    let counts: Vec<[Option<FibreCounts>; 2]> = record
        .patients
        .iter()
        .map(|p| {
            let read = record
                .patient_folder(p)
                .and_then(|dir| read_ea_stats(&stats_path(&dir, &p.subject)).ok());
            let ft = read
                .as_ref()
                .and_then(|r| r.stats.stimulation_of(&label))
                .map(|s| s.ft.clone());
            let side = |i: usize| {
                ft.as_ref()
                    .and_then(|f| f[i].as_ref())
                    .filter(|f| !f.fibercounts.is_empty())
                    .map(|f| (f.fibercounts.clone(), f.nfibercounts.clone()))
            };
            [side(0), side(1)]
        })
        .collect();
    if counts.iter().all(|c| c.iter().all(Option::is_none)) {
        return Vec::new();
    }
    let n = counts.len();
    let cells = |pick: fn(&FibreCounts) -> &Vec<f64>| Mat5Value::Cell {
        dims: vec![n, 2],
        items: (0..2)
            .flat_map(|side| {
                counts.iter().map(move |c| {
                    c[side]
                        .as_ref()
                        .map_or(Mat5Value::Empty, |v| Mat5Value::row(pick(v)))
                })
            })
            .collect(),
    };
    vec![
        ("fibercounts".to_owned(), cells(|v| &v.0)),
        ("nfibercounts".to_owned(), cells(|v| &v.1)),
    ]
}

/// What an export run did.
#[derive(Debug, Clone, Default)]
pub struct StatsExport {
    pub files: Vec<PathBuf>,
    pub vta_rows: usize,
    pub contact_rows: usize,
    pub summary_rows: usize,
    pub patients_with_vta: usize,
    pub missing_vta: Vec<String>,
    pub notes: Vec<Note>,
}

/// Options of [`write_stats_exports`].
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    pub csv: bool,
    pub lead_dbs_mat: bool,
    pub out_dir: Option<PathBuf>,
}

fn io_error(path: &Path, e: &std::io::Error) -> GroupError {
    GroupError::Io(format!("cannot write {}: {e}", path.display()))
}

pub(super) fn inputs_of(record: &GroupRecord) -> Value {
    let label = record.stim_label();
    let mut inputs = Vec::new();
    for p in record.patients.iter().filter(|p| p.included) {
        let Some(dir) = record.patient_folder(p) else {
            continue;
        };
        let hash = |path: Option<PathBuf>| -> Value {
            path.and_then(|p| file_sha256_hex(&p).ok())
                .map_or(Value::Null, Value::from)
        };
        inputs.push(json!({
            "subject": p.subject,
            "vtastats_sha256": hash(vtastats_path(&dir, subject_id(&p.subject), &label)),
            "stats_mat_sha256": hash(Some(stats_path(&dir, &p.subject))),
        }));
    }
    Value::Array(inputs)
}

pub(super) fn write_table(
    dir: &Path,
    record: &GroupRecord,
    desc: &str,
    table: &Table,
    csv: bool,
    inputs: &Value,
    files: &mut Vec<PathBuf>,
) -> Result<(), GroupError> {
    let tsv = table_path(dir, record, desc, "tsv");
    write_replacing(&tsv, to_tsv(table).as_bytes()).map_err(|e| io_error(&tsv, &e))?;
    files.push(tsv.clone());
    if csv {
        let path = table_path(dir, record, desc, "csv");
        write_replacing(&path, to_csv(table).as_bytes()).map_err(|e| io_error(&path, &e))?;
        files.push(path);
    }
    let sidecar = tsv.with_extension("json");
    let body = json!({
        "software": software_stamp(),
        "guid": record.guid,
        "dataset": record.dataset_name,
        "label": record.stim_label(),
        "atlas_set": record.atlas_set,
        "file": tsv.file_name().map(|n| n.to_string_lossy().into_owned()),
        "columns": table.columns,
        "rows": table.rows.len(),
        "created_at": utc_now_rfc3339(),
        "inputs": inputs,
    });
    let text = serde_json::to_string_pretty(&body)
        .map(|t| t + "\n")
        .map_err(|e| GroupError::Json(e.to_string()))?;
    write_replacing(&sidecar, text.as_bytes()).map_err(|e| io_error(&sidecar, &e))?;
    files.push(sidecar);
    Ok(())
}

/// Builds the VTA, contact distance and summary tables of a group from the subjects' statistics and writes them with their sidecars under `<group folder>/exports/` (or `out_dir`), plus the Lead-DBS statistics export beside the group record when asked.
pub fn write_stats_exports(
    record: &GroupRecord,
    folder: &Path,
    options: &ExportOptions,
) -> Result<StatsExport, GroupError> {
    let VtaLoad {
        subjects,
        missing,
        notes: vta_notes,
    } = load_vta_stats(record);
    let (contacts, contact_notes) = load_contact_rows(record);
    let mut out = StatsExport {
        patients_with_vta: subjects.len(),
        missing_vta: missing,
        notes: vta_notes.into_iter().chain(contact_notes).collect(),
        ..StatsExport::default()
    };
    for m in &out.missing_vta {
        out.notes.push(Note::plain(format!(
            "{m} has no VTA statistics for {}; it is left out of the VTA tables",
            record.stim_label()
        )));
    }
    let dir = options
        .out_dir
        .clone()
        .unwrap_or_else(|| folder.join("exports"));
    let inputs = inputs_of(record);
    let vta = vta_rows(&subjects);
    let summary = summary_rows(&subjects);
    out.vta_rows = vta.len();
    out.contact_rows = contacts.len();
    out.summary_rows = summary.len();
    write_table(
        &dir,
        record,
        "atlasoverlap",
        &vta_table(&vta),
        options.csv,
        &inputs,
        &mut out.files,
    )?;
    write_table(
        &dir,
        record,
        "contactdistance",
        &contact_table(&contacts),
        options.csv,
        &inputs,
        &mut out.files,
    )?;
    write_table(
        &dir,
        record,
        "atlasoverlapsummary",
        &summary_table(&summary),
        options.csv,
        &inputs,
        &mut out.files,
    )?;
    if options.lead_dbs_mat {
        let path = lead_dbs_export_path(options.out_dir.as_deref().unwrap_or(folder), record);
        let mut vars = lead_dbs_export_variables(record, &subjects);
        vars.extend(fibre_count_variables(record));
        let bytes = mat5_bytes(&vars, true).map_err(|e| GroupError::Mat(e.to_string()))?;
        write_replacing(&path, &bytes).map_err(|e| io_error(&path, &e))?;
        out.files.push(path);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "exports_tests.rs"]
mod tests;
