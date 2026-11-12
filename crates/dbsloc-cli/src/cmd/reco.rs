// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use clap::{Args, Subcommand};
use dbsloc_pipeline::bids::Subject;
use dbsloc_pipeline::error::PipelineError;
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::paths;
use dbsloc_pipeline::{
    apply_manual_correction, file_edit_space, inspect_manual_correction, CorrectionRequest,
    EditSpace, SideEdit,
};

use super::{bids_fail, interrupt_flag, output_dir_of};
use crate::fail::{err_line, out_json, CmdResult, Fail};
use crate::run::ants_args::AntsParsed;
use crate::run::options::{build, resolve_choices, RunInputs, StepBackendFlags, REG_BACKENDS};

#[derive(Subcommand)]
pub enum RecoCmd {
    /// print the markers, roll, correction state and edit space of each lead as JSON
    Show(ShowArgs),
    /// move one lead (head / tail, optional roll) and rewrite the reconstruction
    Correct(Box<CorrectArgs>),
}

#[derive(Args)]
pub struct ShowArgs {
    pub dataset_root: PathBuf,
    pub subj_id: String,
    /// electrode model used when the reconstruction records none
    #[arg(long)]
    pub el_model: Option<String>,
    /// find the subject's images by scanning the anat/ folders only, ignoring the rawimages.json index files
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct CorrectArgs {
    pub dataset_root: PathBuf,
    pub subj_id: String,
    /// lead to correct: right or left (needs --head and --tail)
    #[arg(long)]
    pub side: Option<String>,
    /// distal fiducial in millimetres: X,Y,Z
    #[arg(long, value_name = "X,Y,Z", allow_hyphen_values = true)]
    pub head: Option<String>,
    /// proximal fiducial in millimetres: X,Y,Z
    #[arg(long, value_name = "X,Y,Z", allow_hyphen_values = true)]
    pub tail: Option<String>,
    /// mark a lead as checked without changing it: right or left (repeatable)
    #[arg(long, value_name = "SIDE")]
    pub accept: Vec<String>,
    /// roll of a directional lead in degrees (kept from the file when omitted)
    #[arg(long, value_name = "DEG", allow_hyphen_values = true)]
    pub roll: Option<f64>,
    /// coordinate space of head and tail: native or mni (default: the space the file is edited in)
    #[arg(long)]
    pub space: Option<String>,
    /// who made the correction: gui or cli
    #[arg(long, default_value = "cli", value_parser = ["gui", "cli"])]
    pub source: String,
    /// electrode model used when the reconstruction records none
    #[arg(long)]
    pub el_model: Option<String>,
    /// template for the reference-space block and the point warp; default as in `dbsloc run`
    #[arg(long)]
    pub template: Option<PathBuf>,
    /// engine of the native to template point warp: auto, wgpu or ants
    #[arg(long, value_parser = REG_BACKENDS)]
    pub reg_backend: Option<String>,
    /// engine of the native to template point warp (the normalization engine); overrides --reg-backend. Environment variable: DBSLOC_NORMALIZE_BACKEND
    #[arg(long, value_parser = REG_BACKENDS)]
    pub normalize_backend: Option<String>,
    /// find the subject's images by scanning the anat/ folders only, ignoring the rawimages.json index files
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

pub fn run(cmd: RecoCmd) -> CmdResult {
    match cmd {
        RecoCmd::Show(a) => show(&a),
        RecoCmd::Correct(a) => correct(&a),
    }
}

fn manual_fail(reason: impl std::fmt::Display) -> Fail {
    Fail::new(1, format!("dbsloc: error: E_RECON_manual: {reason}"))
}

fn pipeline_fail(e: &PipelineError) -> Fail {
    match e {
        PipelineError::Cancelled => Fail::new(130, "dbsloc: cancelled"),
        PipelineError::StageFailed { message, .. } => {
            Fail::new(1, format!("dbsloc: error: {message}"))
        }
        other => Fail::new(1, format!("dbsloc: error: {other}")),
    }
}

fn show_options(
    root: &std::path::Path,
    output_dir: Option<&std::path::Path>,
    subj_id: &str,
    el_model: Option<&str>,
    no_index: bool,
) -> RunOptions {
    let mut o = RunOptions::new(root.to_path_buf(), subj_id);
    o.output_dir = output_dir_of(output_dir);
    o.el_model = el_model.map(str::to_owned);
    o.use_index = !no_index;
    o.headless = true;
    o.checkreg = false;
    o.no_scene3d = true;
    o
}

fn options_for(a: &CorrectArgs, root: &std::path::Path) -> Result<RunOptions, Fail> {
    let choices = resolve_choices(
        None,
        None,
        a.reg_backend.as_deref(),
        StepBackendFlags {
            normalize: a.normalize_backend.as_deref(),
            ..StepBackendFlags::default()
        },
        None,
    )?;
    build(
        &RunInputs {
            dataset_root: root,
            output_dir: a.output_dir.as_deref(),
            leaddbs_atlases_dir: None,
            postop_modality: None,
            vat_backend: None,
            vat_labels: &[],
            vat_stats_atlas: None,
            subj_id: &a.subj_id,
            stages: Vec::new(),
            template: a.template.as_ref(),
            template_t2: None,
            no_norm_multispectral: false,
            recon_method: "auto",
            recon_bright_metal: false,
            el_model: a.el_model.as_deref(),
            headless: true,
            no_checkreg: true,
            no_scene3d: true,
            plan: false,
            overwrite_approved: false,
            fingerprint_locks: false,
            no_index: a.no_index,
        },
        &choices,
        &AntsParsed::default(),
    )
}

fn subject_of(opts: &RunOptions) -> Result<Subject, Fail> {
    opts.resolve_subject().map_err(|e| bids_fail(&e))
}

fn json_fail(e: serde_json::Error) -> Fail {
    Fail::new(1, format!("dbsloc: error: {e}"))
}

fn show(a: &ShowArgs) -> CmdResult {
    let root = paths::absolute(&a.dataset_root);
    let opts = show_options(
        &root,
        a.output_dir.as_deref(),
        &a.subj_id,
        a.el_model.as_deref(),
        a.no_index,
    );
    let sub = subject_of(&opts)?;
    let ctx = inspect_manual_correction(&sub, &opts).map_err(|e| pipeline_fail(&e))?;
    out_json(&serde_json::to_value(&ctx).map_err(json_fail)?);
    Ok(0)
}

fn parse_xyz(name: &str, raw: &str) -> Result<[f64; 3], Fail> {
    let values: Option<Vec<f64>> = raw
        .split(',')
        .map(|p| p.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect();
    match values.as_deref() {
        Some([x, y, z]) => Ok([*x, *y, *z]),
        _ => Err(manual_fail(format!(
            "--{name} expects X,Y,Z in millimetres, got {raw:?}"
        ))),
    }
}

fn parse_side(raw: &str) -> Result<&'static str, Fail> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "right" => Ok("1"),
        "left" => Ok("2"),
        _ => Err(manual_fail(format!("unknown side {raw:?} (right or left)"))),
    }
}

fn parse_space(raw: &str) -> Result<EditSpace, Fail> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "native" => Ok(EditSpace::Native),
        "mni" => Ok(EditSpace::Mni),
        _ => Err(manual_fail(format!(
            "unknown space {raw:?} (native or mni)"
        ))),
    }
}

fn edits_of(a: &CorrectArgs) -> Result<Vec<SideEdit>, Fail> {
    let mut sides = Vec::new();
    match (&a.side, &a.head, &a.tail) {
        (Some(side), Some(head), Some(tail)) => sides.push(SideEdit {
            key: parse_side(side)?.to_owned(),
            head: parse_xyz("head", head)?,
            tail: parse_xyz("tail", tail)?,
            roll_deg: a.roll,
            accept: false,
        }),
        (None, None, None) if a.roll.is_none() => {}
        _ => {
            return Err(manual_fail(
                "a correction needs --side, --head and --tail together (--roll is optional)",
            ))
        }
    }
    for raw in &a.accept {
        sides.push(SideEdit {
            key: parse_side(raw)?.to_owned(),
            head: [0.0; 3],
            tail: [0.0; 3],
            roll_deg: None,
            accept: true,
        });
    }
    if sides.is_empty() {
        return Err(manual_fail(
            "nothing to do: give --side with --head and --tail, or --accept right|left",
        ));
    }
    Ok(sides)
}

fn correct(a: &CorrectArgs) -> CmdResult {
    let sides = edits_of(a)?;
    let requested = a.space.as_deref().map(parse_space).transpose()?;
    let root = paths::absolute(&a.dataset_root);
    let opts = options_for(a, &root)?;
    let sub = subject_of(&opts)?;
    let space = match requested {
        Some(s) => s,
        None => file_edit_space(&sub).map_err(|e| pipeline_fail(&e))?,
    };
    let req = CorrectionRequest {
        space,
        source: a.source.clone(),
        sides,
    };
    let cancel = interrupt_flag();
    let report = apply_manual_correction(&sub, &opts, &req, Some(&cancel), &mut |line| {
        err_line(&line);
    })
    .map_err(|e| pipeline_fail(&e))?;
    for w in &report.warnings {
        err_line(&format!("dbsloc: warning: reconstruct: {w}"));
    }
    out_json(&serde_json::to_value(&report).map_err(json_fail)?);
    Ok(0)
}
