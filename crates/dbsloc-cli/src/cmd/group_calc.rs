// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use clap::Args;
use dbsloc_pipeline::atlas_catalog::external_atlases_dir_from_env;
use dbsloc_pipeline::group::calc::{
    run_calculate_job, run_contact_distances, CalcEvent, CalcOptions, CalcPhase, CalcReport,
    FlipTransformChoice,
};
use dbsloc_pipeline::group::exports::{write_stats_exports, ExportOptions};
use dbsloc_pipeline::group::{open_group, save_group, GroupError, Note};
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::paths;
use serde_json::{json, Value};

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Args)]
pub struct CalcArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// run only these patients (ids with or without sub-); default: every included patient
    #[arg(long, num_args = 1.., value_name = "ID")]
    pub subjects: Vec<String>,
    /// stop after the first patient that fails; the remaining patients are reported as cancelled
    #[arg(long)]
    pub stop_on_first_failure: bool,
    /// do not compute the contact-to-atlas distances
    #[arg(long)]
    pub no_contact_distances: bool,
    /// also write the mirrored (flipped) stimulation volumes
    #[arg(long)]
    pub mirror: bool,
    /// forward composite of an existing flip transform for --mirror (default: the cached transform of each template space, built first when missing)
    #[arg(long, value_name = "FILE", conflicts_with = "plain_flip")]
    pub flip_transform: Option<PathBuf>,
    /// mirror with the plain x flip only, without the non-linear flip transform (a warning is printed)
    #[arg(long)]
    pub plain_flip: bool,
    /// create a missing group stimulation of a patient from this stimulation label of the same patient
    #[arg(long, value_name = "LABEL")]
    pub copy_settings_from: Option<String>,
    /// replace the group stimulation of a patient by a fresh copy of the --copy-settings-from label whenever they differ
    #[arg(long, requires = "copy_settings_from")]
    pub sync_settings: bool,
    /// folder of the Lead-DBS atlases (default: DBSLOC_LEADDBS_ATLASES_DIR)
    #[arg(long, value_name = "DIR")]
    pub leaddbs_atlases_dir: Option<PathBuf>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct StatsArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// write the tables into this folder instead of <group folder>/exports
    #[arg(long, value_name = "DIR")]
    pub export: Option<PathBuf>,
    /// also write CSV copies of the tables
    #[arg(long)]
    pub csv: bool,
    /// also write the Lead-DBS statistics export (.mat)
    #[arg(long)]
    pub lead_dbs: bool,
    /// do not refresh the contact-to-atlas distances first
    #[arg(long)]
    pub no_contact_distances: bool,
    /// folder of the Lead-DBS atlases (default: DBSLOC_LEADDBS_ATLASES_DIR)
    #[arg(long, value_name = "DIR")]
    pub leaddbs_atlases_dir: Option<PathBuf>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

fn group_fail(e: &GroupError) -> Fail {
    Fail::new(1, format!("dbsloc: error: {e}"))
}

fn template(atlases: Option<&PathBuf>) -> RunOptions {
    let mut run = RunOptions::new(PathBuf::new(), "");
    run.headless = true;
    run.checkreg = false;
    run.no_scene3d = true;
    run.leaddbs_atlases_dir = atlases
        .map(|p| paths::absolute(p))
        .or_else(external_atlases_dir_from_env);
    run
}

fn show_event(ev: &CalcEvent, as_json: bool) {
    match ev {
        CalcEvent::SubjectStarted {
            index,
            total,
            subject,
        } => err_line(&format!("[{}/{total}] {subject}", index + 1)),
        CalcEvent::Log { line, .. } => err_line(line),
        CalcEvent::SubjectFinished(o) => {
            if !as_json {
                let tail = o
                    .message
                    .as_deref()
                    .map_or_else(String::new, |m| format!(": {m}"));
                out_line(&format!("{}  {}{tail}", o.subject, o.phase.as_str()));
            }
        }
    }
}

fn show_notes(notes: &[Note]) {
    for n in notes {
        err_line(&format!("dbsloc: warning: {n}"));
    }
}

fn summary_line(r: &CalcReport) -> String {
    format!(
        "{} subjects: {} computed, {} kept, {} failed, {} cancelled",
        r.outcomes.len(),
        r.count(CalcPhase::Computed),
        r.count(CalcPhase::Kept),
        r.count(CalcPhase::Failed) + r.count(CalcPhase::NotRun),
        r.count(CalcPhase::Cancelled)
    )
}

fn selection(subjects: &[String]) -> Option<Vec<String>> {
    (!subjects.is_empty()).then(|| subjects.to_vec())
}

pub fn calc(a: &CalcArgs) -> CmdResult {
    let cancel = interrupt_flag();
    let mut options = CalcOptions::new(template(a.leaddbs_atlases_dir.as_ref()));
    options.subjects = selection(&a.subjects);
    options.stop_on_first_failure = a.stop_on_first_failure;
    options.contact_distances = !a.no_contact_distances;
    options.mirror = a.mirror;
    options.flip_transform = match (&a.flip_transform, a.plain_flip) {
        (Some(p), _) => FlipTransformChoice::Path(paths::absolute(p)),
        (None, true) => FlipTransformChoice::PlainFlip,
        (None, false) => FlipTransformChoice::Auto,
    };
    options.copy_settings_from.clone_from(&a.copy_settings_from);
    options.sync_settings = a.sync_settings;
    let as_json = a.json;
    let report = run_calculate_job(&a.group_file, &options, Some(&cancel), &mut |ev| {
        show_event(ev, as_json)
    })
    .map_err(|e| group_fail(&e))?;
    show_notes(&report.notes);
    if as_json {
        out_json(&report.to_json());
    } else {
        out_line(&summary_line(&report));
    }
    Ok(report.exit_code())
}

fn export_json(e: &dbsloc_pipeline::group::exports::StatsExport) -> Value {
    json!({
        "files": e.files,
        "vta_rows": e.vta_rows,
        "contact_rows": e.contact_rows,
        "summary_rows": e.summary_rows,
        "patients_with_vta": e.patients_with_vta,
        "missing_vta": e.missing_vta,
    })
}

pub fn stats(a: &StatsArgs) -> CmdResult {
    let cancel = interrupt_flag();
    let mut opened = open_group(&a.group_file).map_err(|e| group_fail(&e))?;
    show_notes(&opened.notes);
    let options = CalcOptions::new(template(a.leaddbs_atlases_dir.as_ref()));
    let as_json = a.json;
    let distances = if a.no_contact_distances {
        None
    } else {
        Some(
            run_contact_distances(
                &mut opened.record,
                &opened.folder,
                &options,
                Some(&cancel),
                &mut |ev| show_event(ev, as_json),
            )
            .map_err(|e| group_fail(&e))?,
        )
    };
    if cancel.load(Ordering::SeqCst) {
        return Ok(130);
    }
    let exported = write_stats_exports(
        &opened.record,
        &opened.folder,
        &ExportOptions {
            csv: a.csv,
            lead_dbs_mat: a.lead_dbs,
            out_dir: a.export.as_deref().map(paths::absolute),
        },
    )
    .map_err(|e| group_fail(&e))?;
    let mut notes = exported.notes.clone();
    if let Some(d) = &distances {
        notes.extend(d.notes.clone());
    }
    match save_group(&opened.json_path, &mut opened.record) {
        Ok(n) => notes.extend(n),
        Err(e) => notes.push(Note::plain(format!(
            "the group file was not fully saved: {e}"
        ))),
    }
    show_notes(&notes);
    if as_json {
        out_json(&json!({
            "contact_distances": distances.as_ref().map(CalcReport::to_json),
            "export": export_json(&exported),
            "warnings": notes.iter().map(|n| json!(n.to_string())).collect::<Vec<_>>(),
        }));
    } else {
        for f in &exported.files {
            out_line(&format!("wrote {}", f.display()));
        }
        out_line(&format!(
            "{} patients with VTA statistics, {} without; {} VTA rows, {} contact rows",
            exported.patients_with_vta,
            exported.missing_vta.len(),
            exported.vta_rows,
            exported.contact_rows
        ));
    }
    let failed = distances
        .as_ref()
        .is_some_and(|d| d.count(CalcPhase::Failed) > 0);
    Ok(i32::from(failed))
}
