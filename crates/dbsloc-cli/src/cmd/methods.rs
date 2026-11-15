// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use dbsloc_pipeline::bids::resolve_subject_in;
use dbsloc_pipeline::{derivatives, methods_text, paths};

use super::{bids_fail, output_dir_of};
use crate::fail::{out_line, CmdResult, Fail};

#[derive(Args)]
pub struct MethodsArgs {
    /// BIDS dataset root (contains rawdata/, dataset_description.json, ...)
    pub dataset_root: PathBuf,
    /// Subject id, with or without sub- prefix
    pub subj_id: String,
    /// print only the most recent entry of each processing step, in the order of each step's latest entry, so a step that was run again appears once
    #[arg(long, conflicts_with = "path")]
    pub last: bool,
    /// print only the path of the methods text file instead of its content
    #[arg(long)]
    pub path: bool,
    /// do not use rawimages / desc-rawimages; scan anat/ only
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

pub fn run(a: &MethodsArgs) -> CmdResult {
    let root = paths::absolute(&a.dataset_root);
    let out = output_dir_of(a.output_dir.as_deref());
    let sub = resolve_subject_in(&root, out.as_deref(), &a.subj_id, !a.no_index)
        .map_err(|e| bids_fail(&e))?;
    let file = derivatives::methods_log(&sub);
    if !file.is_file() {
        return Err(Fail::new(
            1,
            format!(
                "dbsloc: error: no methods text yet for subject {}: {} does not exist; it is written when a processing step completes",
                sub.subj_id,
                file.display()
            ),
        ));
    }
    if a.path {
        out_line(&file.to_string_lossy());
        return Ok(0);
    }
    let bytes = std::fs::read(&file).map_err(|e| {
        Fail::new(
            1,
            format!("dbsloc: error: cannot read {}: {e}", file.display()),
        )
    })?;
    let shown = methods_text::render_bytes(&bytes, a.last);
    if a.last && shown.is_empty() {
        return Err(Fail::new(
            1,
            format!(
                "dbsloc: error: {} contains no methods entry",
                file.display()
            ),
        ));
    }
    let _ = std::io::stdout().write_all(&shown);
    Ok(0)
}
