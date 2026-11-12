// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use clap::Args;
use dbsloc_pipeline::atlas_catalog::external_atlases_dir_from_env;
use dbsloc_pipeline::group::calc::FlipTransformChoice;
use dbsloc_pipeline::group::open_group;
use dbsloc_pipeline::group::view::{parse_normalisation, write_group_view, ColourBy, ViewOptions};
use dbsloc_pipeline::paths;

use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Args)]
pub struct ViewArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// colour the contacts by `group` (default) or by the name of a clinical variable
    #[arg(long, value_name = "group|VARIABLE", default_value = "group")]
    pub colour_by: String,
    /// normalisation of the clinical values per side: none, zscore or van_albada
    #[arg(long, value_name = "NAME", default_value = "none")]
    pub normalisation: String,
    /// add a mirrored copy of every patient (sides swapped and flipped)
    #[arg(long)]
    pub mirror: bool,
    /// template of the group space: a catalogue id or space, or the path of a T1w file (default: the shipped MNI template)
    #[arg(long, value_name = "PATH|ID")]
    pub template: Option<String>,
    /// forward composite of an existing flip transform for --mirror (default: the cached transform; the plain x flip with a warning when none is cached)
    #[arg(long, value_name = "FILE", conflicts_with = "plain_flip")]
    pub flip_transform: Option<PathBuf>,
    /// mirror with the plain x flip only, without the non-linear flip transform
    #[arg(long)]
    pub plain_flip: bool,
    /// show only these patients (ids with or without sub-); default: every included patient
    #[arg(long, num_args = 1.., value_name = "ID")]
    pub subjects: Vec<String>,
    /// folder of the Lead-DBS atlases (default: DBSLOC_LEADDBS_ATLASES_DIR)
    #[arg(long, value_name = "DIR")]
    pub leaddbs_atlases_dir: Option<PathBuf>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

pub fn view(a: &ViewArgs) -> CmdResult {
    let fail = |e: &dyn std::fmt::Display| Fail::new(1, format!("dbsloc: error: {e}"));
    let opened = open_group(&a.group_file).map_err(|e| fail(&e))?;
    let mut options = ViewOptions::new();
    options.colour_by = if a.colour_by.eq_ignore_ascii_case("group") {
        ColourBy::Group
    } else {
        ColourBy::Clinical(a.colour_by.clone())
    };
    options.normalisation = parse_normalisation(&a.normalisation).ok_or_else(|| {
        Fail::new(
            2,
            format!(
                "dbsloc: error: unknown normalisation {:?}; use none, zscore or van_albada",
                a.normalisation
            ),
        )
    })?;
    options.mirror = a.mirror;
    options.template = a.template.clone();
    options.flip_transform = match (&a.flip_transform, a.plain_flip) {
        (Some(p), _) => FlipTransformChoice::Path(paths::absolute(p)),
        (None, true) => FlipTransformChoice::PlainFlip,
        (None, false) => FlipTransformChoice::Auto,
    };
    options.subjects = (!a.subjects.is_empty()).then(|| a.subjects.clone());
    options.leaddbs_atlases_dir = a
        .leaddbs_atlases_dir
        .as_ref()
        .map(|p| paths::absolute(p))
        .or_else(external_atlases_dir_from_env);
    let report =
        write_group_view(&opened.record, &opened.folder, &options).map_err(|e| fail(&e))?;
    for n in opened.notes.iter().chain(&report.notes) {
        err_line(&format!("dbsloc: warning: {n}"));
    }
    for (subject, why) in &report.skipped {
        err_line(&format!("dbsloc: warning: {subject} left out: {why}"));
    }
    if a.json {
        out_json(&report.to_json());
    } else {
        out_line(&report.manifest.display().to_string());
    }
    Ok(0)
}
