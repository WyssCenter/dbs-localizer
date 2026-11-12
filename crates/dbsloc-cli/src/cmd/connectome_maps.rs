// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use clap::Args;
use dbsloc_connectome::FunctionalConnectome;
use dbsloc_pipeline::connectome_catalog::{ConnectomeClass, REFERENCE_SPACE};
use dbsloc_pipeline::connectome_fetch::ConnectomeRoots;
use dbsloc_pipeline::connectome_maps::fibervals::DEFAULT_THRESHOLD_VPM;
use dbsloc_pipeline::connectome_maps::job::{
    resolve_connectome, run_fibervals, run_functional_batch, run_structural, FibervalsRequest,
    FunctionalPass, FunctionalRequest, ResolvedConnectome, Side, Status, StructuralEngine,
    StructuralRequest, SubjectOutcome,
};
use dbsloc_pipeline::connectome_maps::structural::{OutputGrid, SeedKind};
use dbsloc_pipeline::connectome_maps::{MapError, Progress};
use dbsloc_pipeline::paths;
use serde_json::{json, Value};

use super::connectome::RootArg;
use super::{interrupt_flag, output_dir_of};
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Args)]
pub struct MapArgs {
    /// dataset (BIDS) root
    pub dataset_root: PathBuf,
    /// subject ids (without the sub- prefix)
    #[arg(required = true)]
    pub subj_ids: Vec<String>,
    /// stimulation label (the folder under stimulations/<space>/)
    #[arg(long, value_name = "LABEL")]
    pub stim: String,
    /// template space of the stimulation (default MNI152NLin2009bAsym)
    #[arg(long, value_name = "SPACE", default_value = REFERENCE_SPACE)]
    pub space: String,
    /// connectome: catalogue id, installed name or folder, with an optional ' > <subset>'
    #[arg(long, value_name = "CONNECTOME")]
    pub connectome: String,
    /// stimulation image the seed is built from: binary, efield or efieldgauss
    #[arg(long, value_name = "KIND", default_value = "binary", value_parser = parse_kind)]
    pub seed_from: SeedKind,
    /// output grid of a structural map: 2mm, 1mm or 0.5mm
    #[arg(long, value_name = "GRID", default_value = "2mm", value_parser = parse_grid)]
    pub grid: OutputGrid,
    /// restrict the functional output to the voxels of this image
    #[arg(long, value_name = "FILE")]
    pub output_mask: Option<PathBuf>,
    /// also write the correlation map of every connectome subject (functional connectomes)
    #[arg(long)]
    pub per_subject_maps: bool,
    /// use this seed image (one subject only) instead of the stimulation images
    #[arg(long, value_name = "FILE")]
    pub seed: Option<PathBuf>,
    /// stimulation model of the file names when the folder holds several
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// do not write the connected-fibre file of a structural map
    #[arg(long)]
    pub no_fibre_file: bool,
    /// output folder of the pipeline that holds sub-<id>/ (default: <dataset_root>/derivatives/leaddbs). Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    #[command(flatten)]
    pub root: RootArg,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct FibervalsArgs {
    /// dataset (BIDS) root
    pub dataset_root: PathBuf,
    /// subject ids (without the sub- prefix)
    #[arg(required = true)]
    pub subj_ids: Vec<String>,
    /// stimulation label (the folder under stimulations/<space>/)
    #[arg(long, value_name = "LABEL")]
    pub stim: String,
    /// template space of the stimulation (default MNI152NLin2009bAsym)
    #[arg(long, value_name = "SPACE", default_value = REFERENCE_SPACE)]
    pub space: String,
    /// structural connectome: catalogue id, installed name or folder
    #[arg(long, value_name = "CONNECTOME")]
    pub connectome: String,
    /// E-field threshold in V/m (ignored for a binary input, which uses 0.5)
    #[arg(long, value_name = "V/M", default_value_t = DEFAULT_THRESHOLD_VPM)]
    pub threshold: f64,
    /// side to compute: R, L or both
    #[arg(long, value_name = "SIDE", default_value = "both", value_parser = parse_side)]
    pub side: Sides,
    /// image the values are taken from: efield, efieldgauss or binary
    #[arg(long, value_name = "KIND", default_value = "efield", value_parser = parse_kind)]
    pub seed_from: SeedKind,
    /// stimulation model of the file names when the folder holds several
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// output folder of the pipeline that holds sub-<id>/ (default: <dataset_root>/derivatives/leaddbs). Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    #[command(flatten)]
    pub root: RootArg,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sides {
    Right,
    Left,
    Both,
}

fn parse_kind(text: &str) -> Result<SeedKind, String> {
    SeedKind::parse(text).ok_or_else(|| format!("'{text}': expected binary, efield or efieldgauss"))
}

fn parse_grid(text: &str) -> Result<OutputGrid, String> {
    OutputGrid::parse(text).ok_or_else(|| format!("'{text}': expected 2mm, 1mm or 0.5mm"))
}

fn parse_side(text: &str) -> Result<Sides, String> {
    match text.trim() {
        "R" | "r" => Ok(Sides::Right),
        "L" | "l" => Ok(Sides::Left),
        "both" => Ok(Sides::Both),
        other => Err(format!("'{other}': expected R, L or both")),
    }
}

fn fail_of(e: &MapError) -> Fail {
    match e {
        MapError::Cancelled => Fail::new(130, "\ndbsloc: interrupted"),
        MapError::Usage(_) => Fail::new(2, format!("dbsloc: error: {e}")),
        _ => Fail::new(1, format!("dbsloc: error: {e}")),
    }
}

fn progress_printer(json: bool) -> impl Fn(&Progress) + Sync {
    move |p: &Progress| {
        if !json && p.total > 0 {
            err_line(&format!(
                "dbsloc: {} {}/{} {}",
                p.stage, p.done, p.total, p.message
            ));
        }
    }
}

fn summarize(
    json_out: bool,
    conn: &ResolvedConnectome,
    outcomes: &[SubjectOutcome],
    cancelled: bool,
) -> CmdResult {
    let failed = outcomes
        .iter()
        .any(|o| matches!(o.status, Status::Error { .. }));
    if json_out {
        out_json(&json!({
            "connectome": {"id": conn.id, "name": conn.name, "tag": conn.tag, "digest": conn.digest},
            "cancelled": cancelled,
            "subjects": outcomes.iter().map(SubjectOutcome::to_json).collect::<Vec<Value>>(),
        }));
    } else {
        for o in outcomes {
            out_line(&format!("{:<12} {}", o.subject_id, o.status_text()));
            for f in &o.files {
                out_line(&format!("    {}", f.display()));
            }
            for w in &o.warnings {
                err_line(&format!("dbsloc: warning: {w}"));
            }
        }
    }
    if cancelled {
        return Err(Fail::new(130, "\ndbsloc: interrupted"));
    }
    Ok(i32::from(failed))
}

fn connectome_of(connectome: &str, root: &RootArg) -> Result<ResolvedConnectome, Fail> {
    let roots = ConnectomeRoots::resolve(root.root.as_deref())
        .map_err(|e| Fail::new(2, format!("dbsloc: error: {e}")))?;
    resolve_connectome(&roots, connectome).map_err(|e| fail_of(&e))
}

pub fn map(a: &MapArgs) -> CmdResult {
    if a.seed.is_some() && a.subj_ids.len() != 1 {
        return Err(Fail::new(
            2,
            "dbsloc: error: --seed needs exactly one subject",
        ));
    }
    let conn = connectome_of(&a.connectome, &a.root)?;
    let derivatives = paths::derivatives_root(
        &a.dataset_root,
        output_dir_of(a.output_dir.as_deref()).as_deref(),
    );
    let cancel = interrupt_flag();
    let progress = progress_printer(a.json);
    let mut outcomes: Vec<SubjectOutcome> = Vec::new();
    let mut cancelled = false;
    if conn.class == ConnectomeClass::Fmri {
        let functional =
            FunctionalConnectome::open(&conn.path).map_err(|e| fail_of(&MapError::from(e)))?;
        let reqs: Vec<FunctionalRequest> = a
            .subj_ids
            .iter()
            .map(|id| FunctionalRequest {
                derivatives_root: derivatives.clone(),
                subject_id: id.clone(),
                label: a.stim.clone(),
                space: a.space.clone(),
                connectome: conn.clone(),
                kind: a.seed_from,
                model: a.model.clone(),
                seed_file: a.seed.clone(),
                per_subject_maps: a.per_subject_maps,
            })
            .collect();
        let pass = FunctionalPass {
            output_mask: a.output_mask.clone(),
        };
        match run_functional_batch(&functional, &reqs, &pass, &cancel, &progress) {
            Ok(o) => outcomes = o,
            Err(MapError::Cancelled) => cancelled = true,
            Err(e) => return Err(fail_of(&e)),
        }
    } else {
        let engine = StructuralEngine::load_with_cancel(&conn, &cancel, Some(&progress))
            .map_err(|e| fail_of(&e))?;
        print_load_messages(&engine.notes, &engine.warnings);
        for id in &a.subj_ids {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let req = StructuralRequest {
                derivatives_root: derivatives.clone(),
                subject_id: id.clone(),
                label: a.stim.clone(),
                space: a.space.clone(),
                connectome: conn.clone(),
                kind: a.seed_from,
                model: a.model.clone(),
                grid: a.grid,
                seed_file: a.seed.clone(),
                write_subset: !a.no_fibre_file,
            };
            match SubjectOutcome::from_result(id, run_structural(&engine, &req, &cancel, &progress))
            {
                Ok(o) => outcomes.push(o),
                Err(_) => {
                    cancelled = true;
                    break;
                }
            }
        }
    }
    summarize(a.json, &conn, &outcomes, cancelled)
}

fn print_load_messages(notes: &[String], warnings: &[String]) {
    for n in notes {
        eprintln!("dbsloc: note: {n}");
    }
    for w in warnings {
        eprintln!("dbsloc: warning: {w}");
    }
}

pub fn fibervals(a: &FibervalsArgs) -> CmdResult {
    if !a.threshold.is_finite() || a.threshold < 0.0 {
        return Err(Fail::new(
            2,
            "dbsloc: error: --threshold must be a non-negative number",
        ));
    }
    let conn = connectome_of(&a.connectome, &a.root)?;
    if conn.class == ConnectomeClass::Fmri {
        return Err(Fail::new(
            2,
            format!(
                "dbsloc: error: {} is a functional connectome; fibre values need fibres",
                conn.name
            ),
        ));
    }
    let derivatives = paths::derivatives_root(
        &a.dataset_root,
        output_dir_of(a.output_dir.as_deref()).as_deref(),
    );
    let sides = match a.side {
        Sides::Right => vec![Side::Right],
        Sides::Left => vec![Side::Left],
        Sides::Both => vec![Side::Right, Side::Left],
    };
    let cancel = interrupt_flag();
    let progress = progress_printer(a.json);
    let engine = StructuralEngine::load_with_cancel(&conn, &cancel, Some(&progress))
        .map_err(|e| fail_of(&e))?;
    print_load_messages(&engine.notes, &engine.warnings);
    let mut outcomes: Vec<SubjectOutcome> = Vec::new();
    let mut cancelled = false;
    for id in &a.subj_ids {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let req = FibervalsRequest {
            derivatives_root: derivatives.clone(),
            subject_id: id.clone(),
            label: a.stim.clone(),
            space: a.space.clone(),
            connectome: conn.clone(),
            kind: a.seed_from,
            model: a.model.clone(),
            threshold: a.threshold,
            sides: sides.clone(),
        };
        match SubjectOutcome::from_result(id, run_fibervals(&engine, &req, &cancel, &progress)) {
            Ok(o) => outcomes.push(o),
            Err(_) => {
                cancelled = true;
                break;
            }
        }
    }
    summarize(a.json, &conn, &outcomes, cancelled)
}
