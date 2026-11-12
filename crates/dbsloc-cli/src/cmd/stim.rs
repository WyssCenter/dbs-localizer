// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use dbsloc_pipeline::bids::Subject;
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::paths;
use dbsloc_pipeline::sha256::file_sha256_hex;
use dbsloc_pipeline::vat::settings::{
    check, contact_counts, list_labels, load, normalise, read_stimparameters_mat, save,
    stimparameters_mat_path, write_stimparameters_mat, LabelEntry, Mode, Polarity, Side, StimError,
    Stimulation,
};
use serde_json::{json, Value};

use super::{bids_fail, output_dir_of};
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

const DEFAULT_IMPORT_CONTACTS: usize = 8;

#[derive(Subcommand)]
pub enum StimCmd {
    /// list the stimulations of a subject (ours, and Lead-DBS folders that can be imported)
    List(ListArgs),
    /// print one stimulation
    Show(ShowArgs),
    /// validate a stimulation JSON file and write it as the canonical settings of a label
    Set(SetArgs),
    /// import a Lead-DBS sub-<id>_desc-stimparameters.mat as the canonical settings of a label
    Import(ImportArgs),
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
pub struct ListArgs {
    #[command(flatten)]
    pub subject: SubjectArgs,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ShowArgs {
    #[command(flatten)]
    pub subject: SubjectArgs,
    pub label: String,
    /// print the canonical JSON file
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct SetArgs {
    #[command(flatten)]
    pub subject: SubjectArgs,
    pub label: String,
    /// stimulation JSON file (schema dbsloc-stimulation) to validate and store
    #[arg(long, value_name = "FILE")]
    pub from: PathBuf,
}

#[derive(Args)]
pub struct ImportArgs {
    #[command(flatten)]
    pub subject: SubjectArgs,
    /// the stimparameters .mat file (MATLAB v5, v7 or v7.3)
    pub mat_file: PathBuf,
    /// label of the new stimulation (default: the label stored in the file)
    #[arg(long)]
    pub label: Option<String>,
}

pub fn run(cmd: StimCmd) -> CmdResult {
    match cmd {
        StimCmd::List(a) => list(&a),
        StimCmd::Show(a) => show(&a),
        StimCmd::Set(a) => set(&a),
        StimCmd::Import(a) => import(&a),
    }
}

fn stim_fail(e: &StimError) -> Fail {
    match e {
        StimError::Invalid(errors) => Fail::new(
            1,
            errors
                .iter()
                .map(|v| format!("dbsloc: error: {v}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        other => Fail::new(1, format!("dbsloc: error: {other}")),
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

fn state_text(e: &LabelEntry) -> &'static str {
    match e.state {
        dbsloc_pipeline::vat::settings::LabelState::Canonical => "canonical",
        dbsloc_pipeline::vat::settings::LabelState::Importable => "importable",
    }
}

pub fn table_lines(rows: &[LabelEntry]) -> Vec<String> {
    let width = rows.iter().map(|r| r.label.len()).max().unwrap_or(5).max(5);
    let mut lines = vec![format!("{:<width$}  {:<22}  state", "label", "space")];
    for r in rows {
        lines.push(format!(
            "{:<width$}  {:<22}  {}",
            r.label,
            r.space,
            state_text(r)
        ));
    }
    lines
}

fn list(a: &ListArgs) -> CmdResult {
    let sub = subject_of(&a.subject)?;
    let rows = list_labels(&sub.subj_dir, &sub.subj_id);
    if a.json {
        out_json(&serde_json::to_value(&rows).unwrap_or(Value::Null));
    } else {
        for l in table_lines(&rows) {
            out_line(&l);
        }
    }
    Ok(0)
}

pub fn summary_lines(s: &Stimulation) -> Vec<String> {
    let mut lines = vec![
        format!("label: {}", s.label),
        format!("model: {} ({})", s.model.slug(), s.model.display_name()),
        format!(
            "threshold: {} V/mm, conductivity {:?}",
            s.model_settings.threshold_v_per_mm, s.model_settings.conductivity.mode
        ),
    ];
    for side in Side::ALL {
        let active: Vec<_> = s
            .sides
            .get(side)
            .sources
            .iter()
            .filter(|x| x.is_active())
            .collect();
        if active.is_empty() {
            lines.push(format!("{side}: no stimulation"));
            continue;
        }
        for src in active {
            let (mode, unit) = match src.mode {
                Mode::Voltage => ("voltage", "V"),
                Mode::Current => ("current", "mA"),
            };
            let contacts = src
                .contacts
                .iter()
                .filter(|c| c.percent > 0.0)
                .map(|c| {
                    let sign = if c.polarity == Polarity::Anode {
                        '+'
                    } else {
                        '-'
                    };
                    format!("{}{sign}{}%", c.index, c.percent)
                })
                .collect::<Vec<_>>()
                .join(" ");
            let case = if src.case.polarity != Polarity::Off && src.case.percent > 0.0 {
                format!(", case {:?} {}%", src.case.polarity, src.case.percent)
            } else {
                String::new()
            };
            lines.push(format!(
                "{side}: source {}: {mode} {} {unit}, {} us, contacts {contacts}{case}",
                src.index, src.amplitude, src.pulse_width_us
            ));
        }
    }
    lines
}

fn show(a: &ShowArgs) -> CmdResult {
    let sub = subject_of(&a.subject)?;
    let stim = load(&sub.subj_dir, &sub.subj_id, &a.label).map_err(|e| stim_fail(&e))?;
    if a.json {
        out_json(&serde_json::to_value(&stim).unwrap_or(Value::Null));
    } else {
        for l in summary_lines(&stim) {
            out_line(&l);
        }
    }
    Ok(0)
}

fn commit(
    sub: &Subject,
    mut stim: Stimulation,
    reco_counts: Option<[usize; 2]>,
    mut messages: Vec<String>,
) -> CmdResult {
    stim.subject.clone_from(&sub.subj_id);
    let counts = reco_counts.unwrap_or_else(|| stim.contact_counts());
    messages.extend(normalise(&mut stim, counts));
    let report = check(&stim, counts);
    messages.extend(report.warnings);
    for m in &messages {
        err_line(m);
    }
    if !report.errors.is_empty() {
        return Err(stim_fail(&StimError::Invalid(report.errors)));
    }
    let json_path = save(&sub.subj_dir, &stim).map_err(|e| stim_fail(&e))?;
    let saved = load(&sub.subj_dir, &sub.subj_id, &stim.label).map_err(|e| stim_fail(&e))?;
    let mat_path = stimparameters_mat_path(&sub.subj_dir, &sub.subj_id, "native", &stim.label);
    let warnings =
        write_stimparameters_mat(&mat_path, &saved, [0.0, 0.0]).map_err(|e| stim_fail(&e))?;
    for w in &warnings {
        err_line(w);
    }
    out_json(&json!({
        "label": stim.label,
        "json": json_path.display().to_string(),
        "mat": mat_path.display().to_string(),
        "contacts_per_side": counts,
    }));
    Ok(0)
}

fn set(a: &SetArgs) -> CmdResult {
    let sub = subject_of(&a.subject)?;
    let text = std::fs::read_to_string(&a.from).map_err(|e| {
        Fail::new(
            1,
            format!("dbsloc: error: cannot read {}: {e}", a.from.display()),
        )
    })?;
    let stim: Stimulation = serde_json::from_str(&text).map_err(|e| {
        Fail::new(
            1,
            format!(
                "dbsloc: error: vat: E_VAT_settings: {}: {e}",
                a.from.display()
            ),
        )
    })?;
    if stim.label != a.label {
        return Err(Fail::new(
            1,
            format!(
                "dbsloc: error: vat: E_VAT_settings: the file holds label {:?}, not {:?}",
                stim.label, a.label
            ),
        ));
    }
    let counts = contact_counts(&sub).map_err(|e| stim_fail(&e))?;
    commit(&sub, stim, counts, Vec::new())
}

fn label_from_path(mat: &Path) -> String {
    mat.parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn import(a: &ImportArgs) -> CmdResult {
    let sub = subject_of(&a.subject)?;
    let counts = contact_counts(&sub).map_err(|e| stim_fail(&e))?;
    let fallback = counts
        .map(|c| c[0].max(c[1]))
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_IMPORT_CONTACTS);
    let mat = paths::absolute(&a.mat_file);
    let (mut stim, notes) =
        read_stimparameters_mat(&mat, &sub.subj_id, fallback).map_err(|e| stim_fail(&e))?;
    match &a.label {
        Some(l) => stim.label.clone_from(l),
        None if stim.label.is_empty() => stim.label = label_from_path(&mat),
        None => {}
    }
    let sha = file_sha256_hex(&mat).map_err(|e| {
        Fail::new(
            1,
            format!("dbsloc: error: cannot read {}: {e}", mat.display()),
        )
    })?;
    stim.provenance.imported_from = Some(json!({
        "path": mat.display().to_string(),
        "sha256": sha,
    }));
    commit(&sub, stim, counts, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbsloc_pipeline::vat::settings::{LabelState, ModelKind};

    #[test]
    fn table_has_header_and_one_line_per_entry() {
        let rows = vec![LabelEntry {
            label: "a".into(),
            space: "native".into(),
            state: LabelState::Canonical,
        }];
        let t = table_lines(&rows);
        assert_eq!(t.len(), 2);
        assert!(t[0].starts_with("label"));
        assert!(t[1].contains("canonical"));
    }

    #[test]
    fn summary_names_the_model_and_idle_sides() {
        let s = Stimulation::blank("x", "1", ModelKind::Efield, [4, 4]);
        let l = summary_lines(&s);
        assert!(l.iter().any(|x| x == "right: no stimulation"));
        assert!(l.iter().any(|x| x.starts_with("model: simbio")));
    }
}
