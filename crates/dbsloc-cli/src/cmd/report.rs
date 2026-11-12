// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use dbsloc_pipeline::bids::Subject;
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::paths;
use dbsloc_pipeline::reports::model::{Group, Hemisphere, Polarity, Program, StimMode};
use dbsloc_pipeline::reports::{
    anonymize_file, anonymize_preview, free_text_warning, import_report, inspect, list_reports,
    ImportOptions, IndexEntry, ReportImportError, SessionReport, SnapshotChoice,
};
use serde_json::{json, Value};

use super::{bids_fail, output_dir_of};
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum ReportCmd {
    /// parse a vendor session report PDF and print what it holds, without writing anything
    Inspect(InspectArgs),
    /// import session report PDFs into a subject: record, source copy, stimulation labels
    Import(ImportArgs),
    /// list the session reports imported into a subject
    List(ListArgs),
    /// parse a PDF and write the anonymised record to a file outside any dataset
    Anonymize(AnonymizeArgs),
}

#[derive(Args)]
pub struct InspectArgs {
    pub pdf: PathBuf,
    /// print the record as JSON
    #[arg(long)]
    pub json: bool,
    /// show what the anonymised record would contain (dates unshifted, no key needed)
    #[arg(long)]
    pub anonymize_preview: bool,
}

#[derive(Args)]
pub struct SubjectArgs {
    pub dataset_root: PathBuf,
    pub subj_id: String,
    /// find the subject's images by scanning the anat/ folders only, ignoring the rawimages.json index files
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct ImportArgs {
    #[command(flatten)]
    pub subject: SubjectArgs,
    #[arg(required = true, value_name = "PDF")]
    pub pdfs: Vec<PathBuf>,
    /// remove name, patient id, notes and serial number; shift every date by a fixed random offset of at most 30 days
    #[arg(long)]
    pub anonymize: bool,
    /// snapshot that becomes stimulation labels: final, initial, both or none
    #[arg(long, value_name = "WHICH", default_value = "final", value_parser = parse_snapshot)]
    pub snapshot: SnapshotChoice,
    /// group ids to import (default: every group with a program above 0 amplitude)
    #[arg(long, value_name = "A,B", value_delimiter = ',')]
    pub groups: Option<Vec<String>>,
    /// prefix of every label
    #[arg(long, value_name = "PREFIX")]
    pub label_prefix: Option<String>,
    /// do not keep a copy of the PDF in sourcedata
    #[arg(long)]
    pub no_source_copy: bool,
    /// anonymisation key file (default: DBSLOC_ANONYMIZATION_KEY_FILE, else anonymization.key in the user data folder)
    #[arg(long, value_name = "FILE")]
    pub key_file: Option<PathBuf>,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub subject: SubjectArgs,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct AnonymizeArgs {
    pub pdf: PathBuf,
    /// subject id the offset is derived for, as sub-<id>
    #[arg(long, value_name = "SUB-ID")]
    pub subject: String,
    /// file the anonymised record is written to
    #[arg(long, value_name = "JSON")]
    pub out: PathBuf,
    /// anonymisation key file (default: DBSLOC_ANONYMIZATION_KEY_FILE, else anonymization.key in the user data folder)
    #[arg(long, value_name = "FILE")]
    pub key_file: Option<PathBuf>,
}

fn parse_snapshot(text: &str) -> Result<SnapshotChoice, String> {
    SnapshotChoice::parse(text).ok_or_else(|| "expected final, initial, both or none".to_owned())
}

pub fn run(cmd: ReportCmd) -> CmdResult {
    match cmd {
        ReportCmd::Inspect(a) => inspect_cmd(&a),
        ReportCmd::Import(a) => import_cmd(&a),
        ReportCmd::List(a) => list_cmd(&a),
        ReportCmd::Anonymize(a) => anonymize_cmd(&a),
    }
}

fn report_fail(e: &dyn std::fmt::Display) -> Fail {
    Fail::new(1, format!("dbsloc: error: report: {e}"))
}

fn first_lines_text(e: &ReportImportError) -> Option<String> {
    let ReportImportError::Report(inner) = e else {
        return None;
    };
    let lines = inner.first_lines()?;
    Some(format!(
        "dbsloc: note: report: first lines of page 1: {}",
        if lines.is_empty() {
            "none".to_owned()
        } else {
            lines.join(" | ")
        }
    ))
}

fn inspect_fail(e: &ReportImportError) -> Fail {
    match first_lines_text(e) {
        Some(lines) => Fail::new(1, format!("dbsloc: error: report: {e}\n{lines}")),
        None => report_fail(e),
    }
}

fn subject_of(a: &SubjectArgs) -> Result<Subject, Fail> {
    let root = paths::absolute(&a.dataset_root);
    let mut o = RunOptions::new(root, &a.subj_id);
    o.output_dir = output_dir_of(a.output_dir.as_deref());
    o.use_index = !a.no_index;
    o.headless = true;
    o.checkreg = false;
    o.no_scene3d = true;
    o.resolve_subject().map_err(|e| bids_fail(&e))
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1.0e12 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn contact_text(program: &Program) -> String {
    let mut parts: Vec<String> = program
        .contacts
        .iter()
        .map(|c| {
            let sign = match c.polarity {
                Polarity::Cathode => '-',
                Polarity::Anode => '+',
                Polarity::Off => '.',
            };
            match c.amplitude {
                Some(a) => format!("{}{sign}({})", c.label, number(a)),
                None => format!("{}{sign}", c.label),
            }
        })
        .collect();
    let case = match program.case {
        Some(Polarity::Cathode) => "case-",
        Some(Polarity::Anode) => "case+",
        _ => "case.",
    };
    parts.push(case.to_owned());
    parts.join(" ")
}

fn program_line(hemisphere: &str, group: &Group, program: &Program) -> String {
    let unit = program
        .unit
        .as_deref()
        .unwrap_or(if program.mode == Some(StimMode::Voltage) {
            "V"
        } else {
            "mA"
        });
    let amplitude = program
        .amplitude
        .map_or_else(|| "?".to_owned(), |a| format!("{} {unit}", number(a)));
    let pulse = program
        .pulse_width_us
        .map_or_else(|| "?".to_owned(), |p| format!("{} us", number(p)));
    let rate = program
        .frequency_hz
        .map_or_else(|| "?".to_owned(), |f| format!("{} Hz", number(f)));
    format!(
        "{:<5} {:<6} {:>2}  {:<10} {:<8} {:<8} {}",
        group.id,
        hemisphere,
        program.index,
        amplitude,
        pulse,
        rate,
        contact_text(program)
    )
}

pub fn inspect_lines(r: &SessionReport) -> Vec<String> {
    let mut lines = vec![
        format!("vendor: {} ({})", r.vendor.as_str(), r.report_kind),
        format!(
            "file: {} ({} pages, sha256 {})",
            r.source.filename,
            r.source.pages,
            r.source.sha256.get(..12).unwrap_or(&r.source.sha256)
        ),
        format!(
            "session: {} (stimulation initial {}, final {})",
            r.session.date.as_deref().unwrap_or("unknown"),
            r.session.stimulation_initial.as_deref().unwrap_or("?"),
            r.session.stimulation_final.as_deref().unwrap_or("?"),
        ),
        format!(
            "device: {} {}, firmware {}",
            r.device.model.as_deref().unwrap_or("unknown"),
            r.device
                .model_number
                .as_deref()
                .map(|n| format!("({n})"))
                .unwrap_or_default(),
            r.device.firmware.product_version.as_deref().unwrap_or("?"),
        ),
    ];
    for lead in &r.leads {
        let side = match lead.hemisphere {
            Hemisphere::Left => "left",
            Hemisphere::Right => "right",
        };
        lines.push(format!(
            "lead {side}: {} (vendor model {}), target {}, {} contacts",
            lead.catalogue_model.as_deref().unwrap_or("unknown"),
            lead.vendor_model.as_deref().unwrap_or("?"),
            lead.target.as_deref().unwrap_or("?"),
            lead.contacts.len()
        ));
    }
    lines.push(format!(
        "final snapshot (active group {}):",
        r.snapshots.final_.active_group.as_deref().unwrap_or("none")
    ));
    lines.push(format!(
        "{:<5} {:<6} {:>2}  {:<10} {:<8} {:<8} contacts",
        "group", "side", "p", "amplitude", "width", "rate"
    ));
    for group in &r.snapshots.final_.groups {
        for (name, settings) in [
            ("left", &group.hemispheres.left),
            ("right", &group.hemispheres.right),
        ] {
            for program in settings.iter().flat_map(|h| h.programs.iter()) {
                lines.push(program_line(name, group, program));
            }
        }
    }
    if r.diagnostics.is_empty() {
        lines.push("diagnostics: none".to_owned());
    } else {
        lines.push("diagnostics:".to_owned());
        for d in &r.diagnostics {
            let path = d
                .path
                .as_deref()
                .map(|p| format!(" [{p}]"))
                .unwrap_or_default();
            lines.push(format!(
                "  {:?} {}{path}: {}",
                d.severity, d.code, d.message
            ));
        }
    }
    lines
}

fn inspect_cmd(a: &InspectArgs) -> CmdResult {
    let record = inspect(&a.pdf).map_err(|e| {
        if a.anonymize_preview {
            report_fail(&e)
        } else {
            inspect_fail(&e)
        }
    })?;
    let record = if a.anonymize_preview {
        anonymize_preview(&record)
    } else {
        record
    };
    if a.json {
        out_json(&serde_json::to_value(&record).unwrap_or(Value::Null));
    } else {
        for l in inspect_lines(&record) {
            out_line(&l);
        }
    }
    Ok(0)
}

fn import_one(subject: &Subject, pdf: &Path, opts: &ImportOptions) -> (Value, bool) {
    let name = pdf.display().to_string();
    match import_report(subject, pdf, opts) {
        Ok(rep) => {
            for w in &rep.warnings {
                err_line(w);
            }
            for (label, reason) in &rep.labels_failed {
                err_line(&format!("dbsloc: error: report: {label}: {reason}"));
            }
            let ok = rep.labels_failed.is_empty();
            let mut v = serde_json::to_value(&rep).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.insert("pdf".into(), json!(name));
                o.insert("ok".into(), json!(ok));
            }
            (v, ok)
        }
        Err(e) => {
            err_line(&format!("dbsloc: error: report: {e}"));
            (
                json!({"pdf": name, "ok": false, "error": e.to_string()}),
                false,
            )
        }
    }
}

fn summary_lines(v: &Value) -> Vec<String> {
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
    if v["error"].is_string() {
        return vec![format!("{}: failed", text("pdf"))];
    }
    let list = |k: &str| {
        v[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    vec![
        format!("{}: record {}", text("pdf"), text("record_path")),
        format!("  labels written: {}", list("labels_written")),
        format!("  labels unchanged: {}", list("labels_unchanged")),
    ]
}

fn import_cmd(a: &ImportArgs) -> CmdResult {
    let subject = subject_of(&a.subject)?;
    let opts = ImportOptions {
        anonymize: a.anonymize,
        snapshot: a.snapshot,
        groups: a.groups.clone(),
        label_prefix: a.label_prefix.clone(),
        source_copy: !a.no_source_copy,
        key_file: a.key_file.as_deref().map(paths::absolute),
    };
    let mut results = Vec::new();
    let mut all_ok = true;
    for pdf in &a.pdfs {
        let (value, ok) = import_one(&subject, &paths::absolute(pdf), &opts);
        all_ok &= ok;
        if !a.json {
            for l in summary_lines(&value) {
                out_line(&l);
            }
        }
        results.push(value);
    }
    if a.json {
        out_json(&json!({"results": results}));
    }
    Ok(i32::from(!all_ok))
}

fn list_lines(rows: &[IndexEntry]) -> Vec<String> {
    let width = rows
        .iter()
        .map(|r| r.folder.len())
        .max()
        .unwrap_or(6)
        .max(6);
    let mut lines = vec![format!(
        "{:<width$}  {:<16}  {:<10}  labels",
        "folder", "session", "anonymized"
    )];
    for r in rows {
        lines.push(format!(
            "{:<width$}  {:<16}  {:<10}  {}",
            r.folder,
            r.session.as_deref().unwrap_or("unknown"),
            if r.anonymized { "yes" } else { "no" },
            r.labels.join(", ")
        ));
    }
    lines
}

fn list_cmd(a: &ListArgs) -> CmdResult {
    let subject = subject_of(&a.subject)?;
    let rows = list_reports(&subject);
    if a.json {
        out_json(&json!({"reports": rows}));
    } else {
        for l in list_lines(&rows) {
            out_line(&l);
        }
    }
    Ok(0)
}

fn anonymize_cmd(a: &AnonymizeArgs) -> CmdResult {
    let subject = a.subject.trim();
    let patient_key = if subject.starts_with("sub-") {
        subject.to_owned()
    } else {
        format!("sub-{subject}")
    };
    let out = paths::absolute(&a.out);
    let key_file = a.key_file.as_deref().map(paths::absolute);
    let record = anonymize_file(&a.pdf, &patient_key, key_file.as_deref(), &out)
        .map_err(|e| report_fail(&e))?;
    if let Some(w) = free_text_warning(&record) {
        err_line(&w);
    }
    out_line(&format!("wrote {}", out.display()));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbsloc_pipeline::reports::model::Vendor;

    #[test]
    fn blank_record_prints_header_and_no_diagnostics() {
        let r = SessionReport::blank(Vendor::Medtronic, "percept_session_report");
        let lines = inspect_lines(&r);
        assert!(lines[0].starts_with("vendor: medtronic"));
        assert!(lines.iter().any(|l| l == "diagnostics: none"));
    }

    #[test]
    fn list_has_a_header_and_one_row_per_entry() {
        let rows = vec![IndexEntry {
            folder: "medtronic/202609300851_abcd1234".into(),
            vendor: "medtronic".into(),
            session: Some("2026-09-30T08:51".into()),
            anonymized: false,
            sha256: "x".into(),
            labels: vec!["a".into(), "b".into()],
        }];
        let l = list_lines(&rows);
        assert_eq!(l.len(), 2);
        assert!(l[1].ends_with("a, b"));
    }

    #[test]
    fn snapshot_values_parse() {
        assert_eq!(parse_snapshot("none"), Ok(SnapshotChoice::None));
        assert!(parse_snapshot("x").is_err());
    }
}
