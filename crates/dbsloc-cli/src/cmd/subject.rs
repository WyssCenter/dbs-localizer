// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use clap::{Args, Subcommand};
use dbsloc_pipeline::bids::resolve_subject_in;
use dbsloc_pipeline::paths;
use serde_json::{json, Map, Value};

use super::{bids_fail, output_dir_of};
use crate::fail::{out_json, CmdResult, Fail};
use crate::spacedef;

#[derive(Subcommand)]
pub enum SubjectCmd {
    /// resolve subject paths (JSON)
    Show(ShowArgs),
}

#[derive(Args)]
pub struct ShowArgs {
    /// BIDS dataset root (contains rawdata/, dataset_description.json, ...)
    pub dataset_root: PathBuf,
    /// Subject id, with or without sub- prefix
    pub subj_id: String,
    /// do not use rawimages / desc-rawimages; scan anat/ only
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

const KEYS: [&str; 6] = [
    "subj_id",
    "subj_dir",
    "anchor_modality",
    "postop_modality",
    "preop_raw",
    "postop_raw",
];

pub fn run(cmd: SubjectCmd) -> CmdResult {
    match cmd {
        SubjectCmd::Show(a) => show(&a),
    }
}

fn show(a: &ShowArgs) -> CmdResult {
    let root = paths::absolute(&a.dataset_root);
    let out = output_dir_of(a.output_dir.as_deref());
    let sub = resolve_subject_in(&root, out.as_deref(), &a.subj_id, !a.no_index)
        .map_err(|e| bids_fail(&e))?;
    let full = sub.to_json();
    let mut out = Map::new();
    for k in KEYS {
        out.insert(k.to_owned(), full.get(k).cloned().unwrap_or(Value::Null));
    }
    let sp = spacedef::load(&root.join("spacedef.json"))
        .map_err(|e| Fail::new(1, format!("dbsloc: error: {e}")))?;
    if let Some(sp) = sp {
        out.insert(
            "primary_template".into(),
            json!(spacedef::primary_template_stem(&sub.anchor_modality, &sp)),
        );
        out.insert("space_name".into(), json!(sp.name));
    }
    out_json(&Value::Object(out));
    Ok(0)
}
