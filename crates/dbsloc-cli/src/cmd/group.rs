// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use dbsloc_pipeline::connectome_maps::{MapError, Progress};
use dbsloc_pipeline::group::atlas_stats::{
    run_clinical_stats, side_combination, ClinicalStatsOptions, DEFAULT_PERMUTATIONS, DEFAULT_SEED,
};
use dbsloc_pipeline::group::clinical::{
    delimiter_for, import_table, write_clinical_export, ImportOptions,
};
use dbsloc_pipeline::group::fibrefilter::{
    run_fibrefilter, AnalysisError, AnalysisOutputs, CvScheme, DisplaySettings, DisplayStrategy,
    FibreMode, FibreRunOptions, FibreSettings, FibreTest, Metric, PredBasis, H0,
};
use dbsloc_pipeline::group::lead_group_mat::{apply_imported_stimulations, read_m};
use dbsloc_pipeline::group::netmap::{
    run_netmap, MapKind, MaskChoice, NetmapRunOptions, NetmapSettings,
};
use dbsloc_pipeline::group::overlap::{CORRELATION_MEASURES, SIDE_COMBINATIONS};
use dbsloc_pipeline::group::record::{save_record, subject_name, twin_path};
use dbsloc_pipeline::group::stats_common::{CorrKind, Correction};
use dbsloc_pipeline::group::{
    create_group, locate, open_group, save_group, GroupError, GroupRecord, Note, Opened,
};
use dbsloc_pipeline::paths;
use dbsloc_pipeline::sha256::file_sha256_hex;
use serde_json::{json, Value};

use super::{interrupt_flag, output_dir_of};
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum GroupCmd {
    /// create a group (record JSON and Lead Group file) under <dataset>/derivatives/leadgroup/<guid>/
    Create(CreateArgs),
    /// add patients of a dataset to a group
    Add(AddArgs),
    /// remove patients from a group
    Remove(RemoveArgs),
    /// put patients into a group number, optionally naming and colouring the group
    SetGroup(SetGroupArgs),
    /// clinical variables of the group
    Clinical {
        #[command(subcommand)]
        cmd: ClinicalCmd,
    },
    /// print the patients, groups and clinical variables of a group
    Info(InfoArgs),
    /// write the Lead Group file and the clinical table of a group
    Export(ExportArgs),
    /// create a group record from a Lead Group analysis file (.mat)
    ImportLeadGroup(ImportLeadGroupArgs),
    /// calculate the group stimulation statistics: contact distances and the VTA statistics of every patient
    Calc(super::group_calc::CalcArgs),
    /// atlas overlap tables and summaries of the group (TSV exports)
    Stats(super::group_calc::StatsArgs),
    /// Pearson / Spearman correlations (permutation p) and two-group tests of an atlas overlap measure with a clinical variable
    Correlate(CorrelateArgs),
    /// sweet-spot mapping of VTAs / E-fields against a clinical variable, with cross-validation
    Sweetspot(Box<super::group_sweetspot::SweetspotArgs>),
    /// fibre filtering: per-fibre statistics, cross-validation and the fibre subset files
    Fibrefilter(FibreFilterArgs),
    /// network mapping: voxel-wise maps over seed maps, similarity prediction
    Netmap(NetmapArgs),
    /// write the scene manifest of the group 3D view and print its path
    View(super::group_view::ViewArgs),
    /// the left-right flip transform used for mirroring
    Fliplr {
        #[command(subcommand)]
        cmd: super::group_fliplr::FliplrCmd,
    },
}

#[derive(Subcommand)]
pub enum ClinicalCmd {
    /// import variables from a TSV or CSV table with a subject column
    Import(ClinicalImportArgs),
    /// write the clinical variables as a TSV table with a JSON description
    Export(ClinicalExportArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// dataset root (a folder that holds derivatives/), or an existing derivatives/leadgroup/<id> folder
    pub dataset_root: PathBuf,
    /// analysis id, letters and digits only (default: the local time as yyyymmddHHMMSS)
    #[arg(long)]
    pub guid: Option<String>,
    /// dataset name used in the file names (default: the name of the dataset folder)
    #[arg(long)]
    pub dataset_name: Option<String>,
    /// free-text title stored in the record
    #[arg(long, default_value = "")]
    pub title: String,
    /// atlas set id stored in the record
    #[arg(long)]
    pub atlas_set: Option<String>,
    /// group folder to use instead of <dataset_root>/derivatives/leadgroup/<guid>
    #[arg(long, value_name = "DIR")]
    pub group_dir: Option<PathBuf>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct AddArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// dataset root of the patients
    pub dataset_root: PathBuf,
    /// subject ids, with or without the sub- prefix
    #[arg(required = true)]
    pub subjects: Vec<String>,
    /// group number of the added patients
    #[arg(long = "group", default_value_t = 1)]
    pub group_number: u32,
    /// output folder of the pipeline that holds sub-<id>/ (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct RemoveArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// subject ids, with or without the sub- prefix
    #[arg(required = true)]
    pub subjects: Vec<String>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct SetGroupArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// group number, 1 or higher
    pub number: u32,
    /// subject ids, with or without the sub- prefix
    #[arg(required = true)]
    pub subjects: Vec<String>,
    /// label of the group
    #[arg(long)]
    pub label: Option<String>,
    /// colour of the group as #RRGGBB
    #[arg(long, value_name = "#RRGGBB")]
    pub color: Option<String>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ClinicalImportArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// TSV or CSV table; the delimiter follows the extension
    pub table: PathBuf,
    /// replace variables that already exist
    #[arg(long)]
    pub replace: bool,
    /// name of the subject column (default: participant_id, subject, sub or the first column)
    #[arg(long, value_name = "NAME")]
    pub subject_column: Option<String>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ClinicalExportArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// output TSV file (default: <group folder>/exports/dataset-<d>_analysis-<guid>_desc-clinical.tsv); the JSON description is written beside it
    #[arg(long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct InfoArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ExportArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// write only this output: twin (the Lead Group file) or clinical (the clinical table)
    #[arg(long, value_parser = ["twin", "clinical"])]
    pub only: Option<String>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct CorrelateArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// clinical variable (per patient or per hemisphere) to correlate with
    #[arg(long, value_name = "NAME")]
    pub variable: String,
    /// overlap measure per structure
    #[arg(long, default_value = "overlap_mm3", value_parser = CORRELATION_MEASURES)]
    pub measure: String,
    /// side combination: both (right + left, a missing side counts 0), right, left or all three
    #[arg(long, default_value = "all", value_parser = ["both", "right", "left", "all"])]
    pub sides: String,
    /// random label permutations for the p-values (all permutations are enumerated for up to 7 points)
    #[arg(long, default_value_t = DEFAULT_PERMUTATIONS, value_name = "N")]
    pub permutations: usize,
    /// seed of the permutations
    #[arg(long, default_value_t = DEFAULT_SEED)]
    pub seed: u64,
    /// write the tables into this folder instead of <group folder>/exports
    #[arg(long, value_name = "DIR")]
    pub export: Option<PathBuf>,
    /// also write CSV copies of the tables
    #[arg(long)]
    pub csv: bool,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ImportLeadGroupArgs {
    /// Lead Group analysis file (.mat, MATLAB v5, v7 or v7.3)
    pub mat_file: PathBuf,
    /// folder of the new group record (default: the folder of the .mat file)
    #[arg(long, value_name = "DIR")]
    pub group_dir: Option<PathBuf>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

pub fn run(cmd: GroupCmd) -> CmdResult {
    match cmd {
        GroupCmd::Create(a) => create(&a),
        GroupCmd::Add(a) => add(&a),
        GroupCmd::Remove(a) => remove(&a),
        GroupCmd::SetGroup(a) => set_group(&a),
        GroupCmd::Clinical { cmd } => match cmd {
            ClinicalCmd::Import(a) => clinical_import(&a),
            ClinicalCmd::Export(a) => clinical_export(&a),
        },
        GroupCmd::Info(a) => info(&a),
        GroupCmd::Export(a) => export(&a),
        GroupCmd::ImportLeadGroup(a) => import_lead_group(&a),
        GroupCmd::Calc(a) => super::group_calc::calc(&a),
        GroupCmd::Stats(a) => super::group_calc::stats(&a),
        GroupCmd::Correlate(a) => correlate(&a),
        GroupCmd::Sweetspot(a) => super::group_sweetspot::sweetspot(&a),
        GroupCmd::Fibrefilter(a) => fibrefilter(&a),
        GroupCmd::Netmap(a) => netmap(&a),
        GroupCmd::View(a) => super::group_view::view(&a),
        GroupCmd::Fliplr { cmd } => super::group_fliplr::run(cmd),
    }
}

fn group_fail(e: &GroupError) -> Fail {
    Fail::new(1, format!("dbsloc: error: {e}"))
}

fn show_notes(notes: &[Note]) {
    for n in notes {
        err_line(&format!("dbsloc: warning: {n}"));
    }
}

fn notes_json(notes: &[Note]) -> Value {
    Value::Array(notes.iter().map(|n| json!(n.to_string())).collect())
}

fn emit(as_json: bool, value: Value, lines: &[String]) {
    if as_json {
        out_json(&value);
    } else {
        for l in lines {
            out_line(l);
        }
    }
}

fn open(path: &Path) -> Result<Opened, Fail> {
    let opened = open_group(path).map_err(|e| group_fail(&e))?;
    show_notes(&opened.notes);
    Ok(opened)
}

fn local_guid() -> String {
    jiff::Zoned::now().strftime("%Y%m%d%H%M%S").to_string()
}

fn create(a: &CreateArgs) -> CmdResult {
    let guid = a.guid.clone().unwrap_or_else(local_guid);
    let root = paths::absolute(&a.dataset_root);
    let location = locate(
        &root,
        &guid,
        a.group_dir.as_deref().map(paths::absolute).as_deref(),
        a.dataset_name.as_deref(),
    )
    .map_err(|e| group_fail(&e))?;
    let (record, json_path, notes) =
        create_group(&location, &a.title, a.atlas_set.as_deref()).map_err(|e| group_fail(&e))?;
    show_notes(&notes);
    let twin = twin_path(&location.folder, &record.dataset_name, &record.guid);
    emit(
        a.json,
        json!({
            "group_file": json_path,
            "folder": location.folder,
            "guid": record.guid,
            "dataset_name": record.dataset_name,
            "lead_group_file": twin,
            "warnings": notes_json(&notes),
        }),
        &[format!("created {}", json_path.display())],
    );
    Ok(0)
}

fn add(a: &AddArgs) -> CmdResult {
    let mut opened = open(&a.group_file)?;
    let root = paths::absolute(&a.dataset_root);
    let deriv = paths::derivatives_root(&root, output_dir_of(a.output_dir.as_deref()).as_deref());
    let dataset = opened
        .record
        .ensure_dataset(&root.to_string_lossy(), &deriv.to_string_lossy());
    let mut added = Vec::new();
    let mut refused = Vec::new();
    let mut notes = Vec::new();
    for s in &a.subjects {
        let name = subject_name(s);
        if !deriv.join(&name).is_dir() {
            notes.push(Note::coded(
                "W_LG_PATIENT_MISSING",
                format!(
                    "the folder of {name} does not exist: {}",
                    deriv.join(&name).display()
                ),
            ));
        }
        match opened.record.add_patient(&name, &dataset, a.group_number) {
            Ok(()) => added.push(name),
            Err(e) => refused.push(json!({"subject": name, "reason": e.to_string()})),
        }
    }
    let saved = save_group(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
    notes.extend(saved);
    show_notes(&notes);
    for r in &refused {
        err_line(&format!(
            "dbsloc: error: {}: {}",
            r["subject"].as_str().unwrap_or(""),
            r["reason"].as_str().unwrap_or("")
        ));
    }
    let lines: Vec<String> = added.iter().map(|s| format!("added {s}")).collect();
    emit(
        a.json,
        json!({"added": added, "refused": refused, "warnings": notes_json(&notes)}),
        &lines,
    );
    Ok(i32::from(!refused.is_empty()))
}

fn remove(a: &RemoveArgs) -> CmdResult {
    let mut opened = open(&a.group_file)?;
    let mut removed = Vec::new();
    let mut missing = Vec::new();
    for s in &a.subjects {
        let name = subject_name(s);
        if opened.record.remove_patient(&name) {
            removed.push(name);
        } else {
            missing.push(name);
        }
    }
    let notes = save_group(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
    show_notes(&notes);
    for m in &missing {
        err_line(&format!("dbsloc: error: {m} is not in the group"));
    }
    let lines: Vec<String> = removed.iter().map(|s| format!("removed {s}")).collect();
    emit(
        a.json,
        json!({"removed": removed, "not_in_group": missing, "warnings": notes_json(&notes)}),
        &lines,
    );
    Ok(i32::from(!missing.is_empty()))
}

fn parse_color(text: &str) -> Result<[f64; 3], Fail> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Fail::new(
            1,
            format!("dbsloc: error: --color {text:?} must be #RRGGBB"),
        ));
    }
    let channel =
        |i: usize| f64::from(u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0)) / 255.0;
    Ok([channel(0), channel(1), channel(2)])
}

fn set_group(a: &SetGroupArgs) -> CmdResult {
    let color = a.color.as_deref().map(parse_color).transpose()?;
    let mut opened = open(&a.group_file)?;
    let mut moved = Vec::new();
    let mut missing = Vec::new();
    for s in &a.subjects {
        let name = subject_name(s);
        match opened.record.set_group(&name, a.number) {
            Ok(()) => moved.push(name),
            Err(GroupError::Invalid(m)) if m.contains("not in the group") => missing.push(name),
            Err(e) => return Err(group_fail(&e)),
        }
    }
    if let Some(def) = opened
        .record
        .groups
        .iter_mut()
        .find(|g| g.number == a.number)
    {
        if let Some(label) = &a.label {
            def.label.clone_from(label);
        }
        if color.is_some() {
            def.color = color;
        }
    }
    let notes = save_group(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
    show_notes(&notes);
    for m in &missing {
        err_line(&format!("dbsloc: error: {m} is not in the group"));
    }
    let lines: Vec<String> = moved
        .iter()
        .map(|s| format!("{s} -> group {}", a.number))
        .collect();
    emit(
        a.json,
        json!({"group": a.number, "moved": moved, "not_in_group": missing, "warnings": notes_json(&notes)}),
        &lines,
    );
    Ok(i32::from(!missing.is_empty()))
}

fn clinical_import(a: &ClinicalImportArgs) -> CmdResult {
    let mut opened = open(&a.group_file)?;
    let delimiter = delimiter_for(&a.table).map_err(|e| group_fail(&GroupError::from(e)))?;
    let text = std::fs::read_to_string(&a.table).map_err(|e| {
        Fail::new(
            1,
            format!("dbsloc: error: cannot read {}: {e}", a.table.display()),
        )
    })?;
    let source = a
        .table
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let report = import_table(
        &mut opened.record,
        &text,
        delimiter,
        &ImportOptions {
            replace: a.replace,
            subject_column: a.subject_column.clone(),
            source,
        },
    )
    .map_err(|e| group_fail(&GroupError::from(e)))?;
    let notes = save_group(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
    show_notes(&notes);
    for s in &report.unknown_subjects {
        err_line(&format!(
            "dbsloc: warning: {s} is not in the group; its row was skipped"
        ));
    }
    for s in &report.patients_without_row {
        err_line(&format!(
            "dbsloc: warning: {s} has no row in the table; its values are missing"
        ));
    }
    let mut lines: Vec<String> = report
        .imported
        .iter()
        .map(|n| format!("imported {n}"))
        .collect();
    lines.extend(report.replaced.iter().map(|n| format!("replaced {n}")));
    emit(
        a.json,
        json!({
            "imported": report.imported,
            "replaced": report.replaced,
            "unknown_subjects": report.unknown_subjects,
            "patients_without_row": report.patients_without_row,
            "warnings": notes_json(&notes),
        }),
        &lines,
    );
    Ok(0)
}

fn clinical_export(a: &ClinicalExportArgs) -> CmdResult {
    let opened = open(&a.group_file)?;
    let (tsv, sidecar) = match &a.output {
        None => {
            write_clinical_export(&opened.record, &opened.folder).map_err(|e| group_fail(&e))?
        }
        Some(out) => write_clinical_to(&opened.record, &paths::absolute(out))?,
    };
    emit(
        a.json,
        json!({"table": tsv, "description": sidecar}),
        &[format!("wrote {}", tsv.display())],
    );
    Ok(0)
}

fn write_clinical_to(record: &GroupRecord, tsv: &Path) -> Result<(PathBuf, PathBuf), Fail> {
    let io = |p: &Path, e: std::io::Error| {
        Fail::new(
            1,
            format!("dbsloc: error: cannot write {}: {e}", p.display()),
        )
    };
    if let Some(parent) = tsv.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    }
    std::fs::write(tsv, dbsloc_pipeline::group::clinical::export_tsv(record))
        .map_err(|e| io(tsv, e))?;
    let sidecar = tsv.with_extension("json");
    let name = tsv
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = serde_json::to_string_pretty(&dbsloc_pipeline::group::clinical::export_sidecar(
        record, &name,
    ))
    .map(|t| t + "\n")
    .unwrap_or_default();
    std::fs::write(&sidecar, text).map_err(|e| io(&sidecar, e))?;
    Ok((tsv.to_path_buf(), sidecar))
}

fn info(a: &InfoArgs) -> CmdResult {
    let opened = open(&a.group_file)?;
    let r = &opened.record;
    let twin = twin_path(&opened.folder, &r.dataset_name, &r.guid);
    let twin_sha = file_sha256_hex(&twin).ok();
    let patients: Vec<Value> = r
        .patients
        .iter()
        .map(|p| {
            let folder = r.patient_folder(p);
            let has_reco = folder.as_ref().is_some_and(|f| {
                f.join("reconstruction")
                    .join(format!("{}_desc-reconstruction.json", p.subject))
                    .is_file()
            });
            json!({
                "subject": p.subject,
                "group": p.group,
                "group_label": r.group_label_of(p.group),
                "dataset": p.dataset,
                "included": p.included,
                "folder_exists": folder.as_ref().is_some_and(|f| f.is_dir()),
                "has_reconstruction": has_reco,
            })
        })
        .collect();
    let clinical: Vec<Value> = r
        .clinical
        .iter()
        .map(|v| {
            let missing = v.values.iter().flatten().filter(|x| x.is_none()).count();
            json!({
                "name": v.name,
                "kind": v.kind,
                "columns": v.width(),
                "missing_values": missing,
                "source": v.source,
            })
        })
        .collect();
    let value = json!({
        "guid": r.guid,
        "dataset_name": r.dataset_name,
        "title": r.title,
        "group_file": opened.json_path,
        "folder": opened.folder,
        "stimulation": r.stimulation,
        "atlas_set": r.atlas_set,
        "datasets": r.datasets,
        "patients": patients,
        "groups": r.groups,
        "clinical": clinical,
        "lead_group_file": {
            "path": twin,
            "exists": twin_sha.is_some(),
            "in_sync": twin_sha.as_deref() == Some(r.lead_group.mat_sha256.as_str()),
            "dropped_fields": r.lead_group.dropped_fields,
        },
    });
    let mut lines = vec![
        format!("group {} (dataset {})", r.guid, r.dataset_name),
        format!("{} patients, {} groups", r.patients.len(), r.groups.len()),
    ];
    for p in &r.patients {
        lines.push(format!(
            "  {}  group {} ({})",
            p.subject,
            p.group,
            r.group_label_of(p.group)
        ));
    }
    for v in &r.clinical {
        lines.push(format!("  clinical {}: {} column(s)", v.name, v.width()));
    }
    emit(a.json, value, &lines);
    Ok(0)
}

fn export(a: &ExportArgs) -> CmdResult {
    let mut opened = open(&a.group_file)?;
    let mut outputs = serde_json::Map::new();
    let mut lines = Vec::new();
    let mut notes = Vec::new();
    if a.only.as_deref() != Some("clinical") {
        notes = save_group(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
        let twin = twin_path(
            &opened.folder,
            &opened.record.dataset_name,
            &opened.record.guid,
        );
        lines.push(format!("wrote {}", twin.display()));
        outputs.insert("lead_group_file".to_owned(), json!(twin));
    }
    if a.only.as_deref() != Some("twin") {
        let (tsv, sidecar) =
            write_clinical_export(&opened.record, &opened.folder).map_err(|e| group_fail(&e))?;
        lines.push(format!("wrote {}", tsv.display()));
        outputs.insert("clinical_table".to_owned(), json!(tsv));
        outputs.insert("clinical_description".to_owned(), json!(sidecar));
    }
    show_notes(&notes);
    outputs.insert("warnings".to_owned(), notes_json(&notes));
    emit(a.json, Value::Object(outputs), &lines);
    Ok(0)
}

fn correlate(a: &CorrelateArgs) -> CmdResult {
    let mut opened = open(&a.group_file)?;
    let mut options = ClinicalStatsOptions::new(&a.variable);
    options.measure.clone_from(&a.measure);
    options.sides = if a.sides == "all" {
        SIDE_COMBINATIONS.to_vec()
    } else {
        side_combination(&a.sides).into_iter().collect()
    };
    options.permutations = a.permutations.max(1);
    options.seed = a.seed;
    let out_dir = a.export.as_deref().map(paths::absolute);
    let run = run_clinical_stats(
        &mut opened.record,
        &opened.folder,
        &options,
        a.csv,
        out_dir.as_deref(),
    )
    .map_err(|e| group_fail(&e))?;
    let mut notes = run.notes.clone();
    match save_group(&opened.json_path, &mut opened.record) {
        Ok(n) => notes.extend(n),
        Err(e) => notes.push(Note::plain(format!(
            "the group file was not fully saved: {e}"
        ))),
    }
    show_notes(&notes);
    let mut value = run.stats.to_json();
    if let Value::Object(o) = &mut value {
        o.insert("files".to_owned(), json!(run.files));
        o.insert("analysis_record".to_owned(), json!(run.analysis_record));
        o.insert("missing_vta".to_owned(), json!(run.missing_vta));
        o.insert("warnings".to_owned(), notes_json(&notes));
    }
    let mut lines: Vec<String> = run
        .files
        .iter()
        .map(|f| format!("wrote {}", f.display()))
        .collect();
    lines.push(format!(
        "{} patients with VTA statistics, {} without; {} correlation rows, {} two-group rows",
        run.stats.patients_with_vta,
        run.missing_vta.len(),
        run.stats.correlations.len(),
        run.stats.group_tests.len()
    ));
    emit(a.json, value, &lines);
    Ok(0)
}

fn import_lead_group(a: &ImportLeadGroupArgs) -> CmdResult {
    let mat = paths::absolute(&a.mat_file);
    let read = read_m(&mat).map_err(|e| group_fail(&e))?;
    show_notes(&read.warnings);
    let mut record = read.record;
    let folder = match &a.group_dir {
        Some(d) => paths::absolute(d),
        None => mat.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let json_path =
        dbsloc_pipeline::group::record::record_path(&folder, &record.dataset_name, &record.guid);
    if json_path.exists() {
        return Err(Fail::new(
            1,
            format!("dbsloc: error: {} already exists", json_path.display()),
        ));
    }
    let written =
        apply_imported_stimulations(&record, &read.stimulations).map_err(|e| group_fail(&e))?;
    save_record(&json_path, &mut record).map_err(|e| group_fail(&e))?;
    emit(
        a.json,
        json!({
            "group_file": json_path,
            "guid": record.guid,
            "patients": record.patients.len(),
            "stimulations_written": written,
            "dropped_fields": record.lead_group.dropped_fields,
            "warnings": notes_json(&read.warnings),
        }),
        &[format!("created {}", json_path.display())],
    );
    Ok(0)
}

#[derive(Args)]
pub struct FibreFilterArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// clinical variable used as the outcome
    #[arg(long, value_name = "NAME")]
    pub variable: String,
    /// clinical variable regressed out of the outcome (repeatable)
    #[arg(long, value_name = "NAME")]
    pub covariate: Vec<String>,
    /// structural connectome: catalogue id, installed name or folder
    #[arg(long, value_name = "CONNECTOME")]
    pub connectome: String,
    #[command(flatten)]
    pub root: super::connectome::RootArg,
    /// template space of the stimulation images
    #[arg(long, value_name = "SPACE", default_value = dbsloc_pipeline::connectome_catalog::REFERENCE_SPACE)]
    pub space: String,
    /// stimulation model of the file names when the folder holds several
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// value model: vta (binary), efield or sigmoid
    #[arg(long, default_value = "efield", value_parser = parse_fibre_mode)]
    pub mode: FibreMode,
    /// per-fibre value of the E-field modes: bin, sum, mean, peak or peak5
    #[arg(long, default_value = "peak", value_parser = parse_metric)]
    pub metric: Metric,
    /// test: spearman, pearson, bend, two-sample-t, one-sample-t, rank-sum, signed-rank, reverse-t, proportion, weighted-two-sample, n-map or mean-map
    #[arg(long, default_value = "spearman", value_parser = parse_fibre_test)]
    pub test: FibreTest,
    /// null hypothesis of the one-group tests: average or zero
    #[arg(long, default_value = "average", value_parser = ["average", "zero"])]
    pub h0: String,
    /// E-field threshold in V/m
    #[arg(long, value_name = "V/M", default_value_t = 200.0)]
    pub threshold: f64,
    /// a fibre must be connected in at least this percentage of the patients
    #[arg(long, value_name = "PERCENT", default_value_t = 20.0)]
    pub coverage: f64,
    /// significance filter: none, uncorrected, fdr or bonferroni
    #[arg(long, default_value = "none", value_parser = parse_significance)]
    pub significance: Option<Correction>,
    /// significance level
    #[arg(long, default_value_t = 0.05)]
    pub alpha: f64,
    /// show fibres within this percentage of the peak score (relative to peak)
    #[arg(long, value_name = "PERCENT", default_value_t = 25.0)]
    pub display_percent: f64,
    /// show only the N best fibres per side and sign instead
    #[arg(long, value_name = "N")]
    pub top: Option<usize>,
    /// also show the negative scores
    #[arg(long)]
    pub show_negative: bool,
    /// hide the positive scores
    #[arg(long)]
    pub hide_positive: bool,
    /// prediction basis: mean, sum, peak or peak5
    #[arg(long, default_value = "mean", value_parser = parse_basis)]
    pub basis: PredBasis,
    /// cross-validation: leave-nothing-out, leave-one-out, leave-one-cohort-out or k-fold
    #[arg(long, default_value = "leave-one-out", value_parser = ["leave-nothing-out", "leave-one-out", "leave-one-cohort-out", "k-fold"])]
    pub cv: String,
    /// number of folds of k-fold
    #[arg(long, default_value_t = 5)]
    pub k: usize,
    /// repetitions of k-fold
    #[arg(long, default_value_t = 1)]
    pub k_iterations: usize,
    /// permutations of leave-nothing-out
    #[arg(long, default_value_t = 1000)]
    pub permutations: usize,
    /// random seed of folds and permutations
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// also use the flipped stimulation images
    #[arg(long)]
    pub mirror: bool,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct NetmapArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// clinical variable used as the outcome
    #[arg(long, value_name = "NAME")]
    pub variable: String,
    /// clinical variable regressed out of the outcome (repeatable)
    #[arg(long, value_name = "NAME")]
    pub covariate: Vec<String>,
    /// connectome whose seed maps are used: catalogue id, installed name or folder
    #[arg(long, value_name = "CONNECTOME")]
    pub connectome: String,
    #[command(flatten)]
    pub root: super::connectome::RootArg,
    /// template space of the seed maps
    #[arg(long, value_name = "SPACE", default_value = dbsloc_pipeline::connectome_catalog::REFERENCE_SPACE)]
    pub space: String,
    /// stimulation model of the file names when the folder holds several
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// map: rmap, amap, cmap, nmap or t
    #[arg(long, default_value = "rmap", value_parser = parse_map_kind)]
    pub map: MapKind,
    /// correlation: spearman, pearson or bend
    #[arg(long, default_value = "spearman", value_parser = parse_corr)]
    pub corr: CorrKind,
    /// threshold of the N-map
    #[arg(long, default_value_t = 0.2)]
    pub n_threshold: f64,
    /// also show the negative part
    #[arg(long)]
    pub show_negative: bool,
    /// hide the positive part
    #[arg(long)]
    pub hide_positive: bool,
    /// significance filter: none, uncorrected, fdr or bonferroni
    #[arg(long, default_value = "none", value_parser = parse_significance)]
    pub significance: Option<Correction>,
    /// significance level
    #[arg(long, default_value_t = 0.05)]
    pub alpha: f64,
    /// Fisher z transform of the similarity
    #[arg(long)]
    pub fisher_z: bool,
    /// mask of the similarity: grey, brain, all or a NIfTI file
    #[arg(long, default_value = "grey")]
    pub mask: String,
    /// cross-validation: leave-nothing-out, leave-one-out, leave-one-cohort-out or k-fold
    #[arg(long, default_value = "leave-one-out", value_parser = ["leave-nothing-out", "leave-one-out", "leave-one-cohort-out", "k-fold"])]
    pub cv: String,
    /// number of folds of k-fold
    #[arg(long, default_value_t = 5)]
    pub k: usize,
    /// repetitions of k-fold
    #[arg(long, default_value_t = 1)]
    pub k_iterations: usize,
    /// permutations of leave-nothing-out
    #[arg(long, default_value_t = 1000)]
    pub permutations: usize,
    /// random seed of folds and permutations
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// also use the flipped seed maps
    #[arg(long)]
    pub mirror: bool,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

fn parse_fibre_mode(t: &str) -> Result<FibreMode, String> {
    FibreMode::parse(t).ok_or_else(|| format!("'{t}': expected vta, efield or sigmoid"))
}

fn parse_metric(t: &str) -> Result<Metric, String> {
    Metric::parse(t).ok_or_else(|| format!("'{t}': expected bin, sum, mean, peak or peak5"))
}

fn parse_fibre_test(t: &str) -> Result<FibreTest, String> {
    FibreTest::parse(t).ok_or_else(|| format!("'{t}': unknown test"))
}

fn parse_basis(t: &str) -> Result<PredBasis, String> {
    PredBasis::parse(t).ok_or_else(|| format!("'{t}': expected mean, sum, peak or peak5"))
}

fn parse_map_kind(t: &str) -> Result<MapKind, String> {
    MapKind::parse(t).ok_or_else(|| format!("'{t}': expected rmap, amap, cmap, nmap or t"))
}

fn parse_corr(t: &str) -> Result<CorrKind, String> {
    CorrKind::parse(t).ok_or_else(|| format!("'{t}': expected spearman, pearson or bend"))
}

fn parse_significance(t: &str) -> Result<Option<Correction>, String> {
    if t.trim().eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    Correction::parse(t)
        .map(Some)
        .ok_or_else(|| format!("'{t}': expected none, uncorrected, fdr or bonferroni"))
}

fn cv_scheme(name: &str, k: usize, iterations: usize, permutations: usize) -> CvScheme {
    match name {
        "leave-nothing-out" => CvScheme::LeaveNothingOut { permutations },
        "leave-one-cohort-out" => CvScheme::LeaveOneCohortOut,
        "k-fold" => CvScheme::KFold { k, iterations },
        _ => CvScheme::LeaveOneOut,
    }
}

fn analysis_fail(e: &AnalysisError) -> Fail {
    match e {
        AnalysisError::Cancelled => Fail::new(130, "\ndbsloc: interrupted"),
        AnalysisError::Map(MapError::Usage(_)) => Fail::new(2, format!("dbsloc: error: {e}")),
        _ => Fail::new(1, format!("dbsloc: error: {e}")),
    }
}

fn analysis_progress(json: bool) -> impl Fn(&Progress) + Sync {
    move |p: &Progress| {
        if !json && p.total > 0 {
            err_line(&format!(
                "dbsloc: {} {}/{} {}",
                p.stage, p.done, p.total, p.message
            ));
        }
    }
}

fn register_analysis(
    opened: &mut Opened,
    kind: &str,
    folder: &str,
    outputs: &AnalysisOutputs,
    json_name: &str,
) {
    opened
        .record
        .register_analysis(kind, folder, &outputs.id, json_name);
}

fn fibrefilter(a: &FibreFilterArgs) -> CmdResult {
    let mut opened = open(&a.group_file)?;
    let display = DisplaySettings {
        strategy: match a.top {
            Some(count) => DisplayStrategy::FixedCount { count },
            None => DisplayStrategy::RelativeToPeak {
                percent: a.display_percent,
            },
        },
        show_positive: !a.hide_positive,
        show_negative: a.show_negative,
    };
    let settings = FibreSettings {
        mode: a.mode,
        metric: a.metric,
        test: a.test,
        h0: if a.h0 == "zero" {
            H0::Zero
        } else {
            H0::Average
        },
        efield_threshold: a.threshold,
        coverage_percent: a.coverage,
        significance: a.significance,
        alpha: a.alpha,
        display,
        basis: a.basis,
    };
    let opts = FibreRunOptions {
        variable: a.variable.clone(),
        covariates: a.covariate.clone(),
        connectome: a.connectome.clone(),
        connectome_root: a.root.root.clone(),
        space: a.space.clone(),
        model: a.model.clone(),
        settings,
        scheme: cv_scheme(&a.cv, a.k, a.k_iterations, a.permutations),
        seed: a.seed,
        mirror: a.mirror,
        group_folder: opened.folder.clone(),
        cache: dbsloc_pipeline::connectome_cache::CacheSetting::from_env(),
    };
    let cancel = interrupt_flag();
    let progress = analysis_progress(a.json);
    let run = run_fibrefilter(&opened.record, &opts, &cancel, &progress)
        .map_err(|e| analysis_fail(&e))?;
    register_analysis(
        &mut opened,
        "fiberfiltering",
        "fiberfiltering",
        &run.outputs,
        "fibrefilter.json",
    );
    save_record(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
    show_notes(&run.notes);
    let ev = run.result.evaluation;
    emit(
        a.json,
        json!({
            "id": run.outputs.id,
            "folder": run.outputs.dir,
            "files": run.outputs.files,
            "shown_fibres": run.result.shown.len(),
            "r": ev.r, "p": ev.p, "auc": ev.auc, "n": ev.n,
            "p_perm": run.result.p_perm, "null_r95": run.result.null95,
            "warnings": notes_json(&run.notes),
        }),
        &[
            format!("wrote {}", run.outputs.dir.display()),
            format!(
                "{} fibres shown; {} r = {:.3} (n = {})",
                run.result.shown.len(),
                a.cv,
                ev.r,
                ev.n
            ),
        ],
    );
    Ok(0)
}

fn netmap(a: &NetmapArgs) -> CmdResult {
    let mut opened = open(&a.group_file)?;
    let mask = match a.mask.as_str() {
        "grey" => MaskChoice::Grey,
        "brain" => MaskChoice::Brain,
        "all" => MaskChoice::All,
        file => MaskChoice::File(paths::absolute(Path::new(file))),
    };
    let settings = NetmapSettings {
        map: a.map,
        corr: a.corr,
        n_threshold: a.n_threshold,
        show_positive: !a.hide_positive,
        show_negative: a.show_negative,
        significance: a.significance,
        alpha: a.alpha,
        fisher_z: a.fisher_z,
    };
    let opts = NetmapRunOptions {
        variable: a.variable.clone(),
        covariates: a.covariate.clone(),
        connectome: a.connectome.clone(),
        connectome_root: a.root.root.clone(),
        space: a.space.clone(),
        model: a.model.clone(),
        settings,
        mask,
        atlases_dir: dbsloc_pipeline::atlas_catalog::external_atlases_dir_from_env(),
        scheme: cv_scheme(&a.cv, a.k, a.k_iterations, a.permutations),
        seed: a.seed,
        mirror: a.mirror,
        group_folder: opened.folder.clone(),
    };
    let cancel = interrupt_flag();
    let progress = analysis_progress(a.json);
    let run =
        run_netmap(&opened.record, &opts, &cancel, &progress).map_err(|e| analysis_fail(&e))?;
    register_analysis(
        &mut opened,
        "networkmapping",
        "networkmapping",
        &run.outputs,
        "netmap.json",
    );
    save_record(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
    show_notes(&run.notes);
    let ev = run.result.evaluation;
    emit(
        a.json,
        json!({
            "id": run.outputs.id,
            "folder": run.outputs.dir,
            "files": run.outputs.files,
            "r": ev.r, "p": ev.p, "auc": ev.auc, "n": ev.n,
            "p_perm": run.result.p_perm, "null_r95": run.result.null95,
            "warnings": notes_json(&run.notes),
        }),
        &[
            format!("wrote {}", run.outputs.dir.display()),
            format!("{} r = {:.3} (n = {})", a.cv, ev.r, ev.n),
        ],
    );
    Ok(0)
}
