// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use clap::{Args, Subcommand};
use dbsloc_pipeline::bids::resolve_subject_in;
use dbsloc_pipeline::locks::{read_approval, stage_approval_path, write_approval};
use dbsloc_pipeline::paths;
use dbsloc_pipeline::stage_names;
use dbsloc_pipeline::stages::Stage;
use serde_json::{json, Value};

use super::{bids_fail, output_dir_of};
use crate::fail::{out_json, CmdResult, Fail};

#[derive(Subcommand)]
pub enum LockCmd {
    /// write <stage>.json with optional inputs
    Approve(ApproveArgs),
    /// read one stage lock (JSON on stdout)
    Show(ShowArgs),
}

#[derive(Args)]
pub struct ApproveArgs {
    pub dataset_root: PathBuf,
    pub subj_id: String,
    /// stage name (e.g. preprocess, normalize)
    pub stage: String,
    /// input file; repeat; stored as inputs[] (content hash) for invalidation
    #[arg(long = "input", value_name = "PATH", action = clap::ArgAction::Append)]
    pub input: Vec<PathBuf>,
    /// optional string stored as method (engine id)
    #[arg(long)]
    pub method: Option<String>,
    /// find the subject's images by scanning the anat/ folders only, ignoring the rawimages.json index files
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct ShowArgs {
    pub dataset_root: PathBuf,
    pub subj_id: String,
    pub stage: String,
    /// find the subject's images by scanning the anat/ folders only, ignoring the rawimages.json index files
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

fn parse_stage(name: &str) -> Result<Stage, Fail> {
    Stage::parse(name).ok_or_else(|| {
        Fail::new(
            2,
            format!(
                "dbsloc: error: unknown stage {name:?}; valid: {}",
                stage_names().join(", ")
            ),
        )
    })
}

pub fn run(cmd: LockCmd) -> CmdResult {
    match cmd {
        LockCmd::Approve(a) => approve(&a),
        LockCmd::Show(a) => show(&a),
    }
}

fn approve(a: &ApproveArgs) -> CmdResult {
    let stage = parse_stage(&a.stage)?;
    let root = paths::absolute(&a.dataset_root);
    let out = output_dir_of(a.output_dir.as_deref());
    let sub = resolve_subject_in(&root, out.as_deref(), &a.subj_id, !a.no_index)
        .map_err(|e| bids_fail(&e))?;
    let p = stage_approval_path(&sub.locks_dir(), stage.cli_name());
    write_approval(&p, true, a.method.as_deref(), &a.input, None)
        .map_err(|e| Fail::new(1, format!("dbsloc: error: {}: {e}", p.display())))?;
    out_json(&json!({
        "wrote": p.to_string_lossy(),
        "stage": stage.cli_name(),
        "input_count": a.input.len(),
    }));
    Ok(0)
}

fn show(a: &ShowArgs) -> CmdResult {
    let stage = parse_stage(&a.stage)?;
    let root = paths::absolute(&a.dataset_root);
    let out = output_dir_of(a.output_dir.as_deref());
    let sub = resolve_subject_in(&root, out.as_deref(), &a.subj_id, !a.no_index)
        .map_err(|e| bids_fail(&e))?;
    let p = stage_approval_path(&sub.locks_dir(), stage.cli_name());
    let lock = read_approval(&p).map_err(|e| Fail::new(1, format!("dbsloc: error: {e}")))?;
    out_json(&json!({
        "path": p.to_string_lossy(),
        "lock": lock.unwrap_or(Value::Null),
    }));
    Ok(0)
}
