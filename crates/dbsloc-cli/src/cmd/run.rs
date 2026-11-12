// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod batch;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use clap::{Args, ValueHint};
use dbsloc_pipeline::options::{PostopModality, RunOptions, VatBackend};
use dbsloc_pipeline::stages::Stage;
use dbsloc_pipeline::{plan_run, run_pipeline_channel, stage_names};

use super::{bids_fail, interrupt_flag};
use crate::fail::{err_line, out_json, CmdResult, Fail};
use crate::run::ants_args::AntsParsed;
use crate::run::options::{
    self, parse_norm_grid, NormGrid, RunInputs, LOG_MODES, NORM_METHODS, RECON_METHODS,
    REG_BACKENDS, VAT_BACKENDS,
};
use crate::run::progress::{Friendly, RunReporter};
use crate::run::summary::{self, Summary};

#[derive(Args)]
pub struct RunArgs {
    /// BIDS dataset root
    pub dataset_root: PathBuf,
    /// Subject id(s), with or without the sub- prefix; several ids run one after another, each through all its steps
    #[arg(value_name = "SUBJ_ID", num_args = 0..)]
    pub subj_ids: Vec<String>,

    /// run every subject of the dataset (rawdata/ and the output folder), in list order, instead of the given ids
    #[arg(long, help_heading = "Batch", conflicts_with_all = ["subj_ids", "subjects_file"])]
    pub all_subjects: bool,
    /// text file with one subject id per line (blank lines and lines starting with # are ignored); added after the ids on the command line
    #[arg(long, value_name = "TXT", help_heading = "Batch", value_hint = ValueHint::FilePath)]
    pub subjects_file: Option<PathBuf>,
    /// with several subjects: a failed subject is reported and the next subject runs (the default)
    #[arg(long, help_heading = "Batch", conflicts_with = "stop_on_first_failure")]
    pub continue_on_error: bool,
    /// with several subjects: stop the batch after the first subject that fails; the remaining subjects are reported as cancelled
    #[arg(long, help_heading = "Batch")]
    pub stop_on_first_failure: bool,
    /// do not wait for the registration check: a subject whose registration is not yet approved stops there and is listed as awaiting review, and the batch moves on (implies --headless)
    #[arg(long, help_heading = "Batch")]
    pub defer_review: bool,

    /// find the subject's images by scanning the anat/ folders only, ignoring the rawimages.json index files
    #[arg(long, help_heading = "Stages")]
    pub no_index: bool,
    /// drop normalize stage
    #[arg(long, help_heading = "Stages")]
    pub no_normalize: bool,
    /// drop the rigid alignment to the template (precoreg) and the coregistration (coreg) stages
    #[arg(long, help_heading = "Stages")]
    pub no_coreg: bool,
    /// drop brainshift stage
    #[arg(long, help_heading = "Stages")]
    pub no_brainshift: bool,
    /// drop reconstruct stage
    #[arg(long, help_heading = "Stages")]
    pub no_reconstruct: bool,
    /// drop orientation stage
    #[arg(long, help_heading = "Stages")]
    pub no_orientation: bool,
    /// drop vat stage
    #[arg(long, help_heading = "Stages")]
    pub no_vat: bool,
    /// drop visualize stage
    #[arg(long, help_heading = "Stages")]
    pub no_visualize: bool,
    /// comma-separated stage list in execution order (names in the epilog); overrides all --no-* stage flags
    #[arg(long, value_name = "NAMES", help_heading = "Stages")]
    pub stages: Option<String>,

    /// auto: CT -> a pre-exported PaCER import JSON if present, else TRAC/CORE on the native coregistered CT with points warped to the template; MRI -> TRAC/CORE on the template-space volume. On a CT, trac_core and trac_core_native both run the native path; pacer runs PaCER in native space and warps its points to the template.
    #[arg(
        long,
        default_value = "auto",
        value_parser = RECON_METHODS,
        help_heading = "Reconstruction"
    )]
    pub recon_method: String,
    /// electrode catalogue name (e.g. 'Medtronic 3387'); overrides the subject uiprefs.mat elmodel. Required when the subject has no readable uiprefs.
    #[arg(long, value_name = "NAME", help_heading = "Reconstruction")]
    pub el_model: Option<String>,
    /// reconstruction: trace the lead as bright (high-intensity) metal instead of a dark artefact; try it when tracing fails on a post-op MR image in which the lead appears bright
    #[arg(long, help_heading = "Reconstruction")]
    pub recon_bright_metal: bool,

    /// plan / locks only (no N4, ANTs, or other stage binaries; JSON still lists stages)
    #[arg(long, help_heading = "Execution")]
    pub plan: bool,
    /// do not open the registration review and 3D scene windows; the registration check images and the scene manifest are still written when those stages run (for machines without a display, such as automated tests or remote shells). Environment variable: DBSLOC_HEADLESS=1
    #[arg(long, help_heading = "Execution")]
    pub headless: bool,
    /// skip the registration quality check (checkreg) after coreg / normalize / brainshift
    #[arg(long, help_heading = "Execution")]
    pub no_checkreg: bool,
    /// after reconstruct: write scene manifest but do not open the 3D scene window (see also DBSLOC_OPEN_SCENE3D=0)
    #[arg(long, help_heading = "Execution")]
    pub no_scene3d: bool,

    /// ignore per-stage approval JSON in locks/
    #[arg(long, help_heading = "Locks")]
    pub overwrite_approved: bool,
    /// pass subject input paths into the lock check: an approved stage is skipped only while its inputs keep the recorded content hash
    #[arg(long, help_heading = "Locks")]
    pub fingerprint_locks: bool,

    /// template for SyN normalize: a T1w NIfTI path or a catalogue id such as MNI152NLin2009bAsym_res-0.5mm; default: $DBSLOC_TEMPLATE_T1, then the shipped MNI152NLin2009bAsym_res-1mm
    #[arg(long, help_heading = "Normalization", value_hint = ValueHint::FilePath)]
    pub template: Option<PathBuf>,
    /// T2w template for the second SyN channel when the subject has a pre-op T2w; default: $DBSLOC_TEMPLATE_T2, then t2.nii.gz / t2.nii in the T1 template's directory
    #[arg(long, help_heading = "Normalization", value_hint = ValueHint::FilePath)]
    pub template_t2: Option<PathBuf>,
    /// normalize on the T1w channel only, even when a pre-op T2w and a T2 template exist
    #[arg(long, help_heading = "Normalization")]
    pub no_norm_multispectral: bool,
    /// isotropic voxel size in millimetres of the normalization registration grid: the anatomy and the template are resampled to it before SyN registration, while the normalized CT keeps the template grid. Default ('template'): the template's own voxel size (1.0 for the shipped template, 0.5 for the 0.5 mm catalogue templates); on the GPU a default grid that does not fit the device steps to the finest coarser grid that does (0.1 mm steps up to 1.0, then 1.5, 2.0, 2.5, 3.0), with a warning in the plan. An explicit value is never changed. 'full' registers on the full template grid without resampling. Environment variable: DBSLOC_NORM_ISOTROPIC_MM (same values)
    #[arg(
        long,
        value_name = "MM|full|template",
        value_parser = parse_norm_grid,
        allow_hyphen_values = true,
        help_heading = "Normalization"
    )]
    pub norm_isotropic_mm: Option<NormGrid>,
    /// normalization method (default: ants_syn). Environment variable: DBSLOC_NORM_METHOD (same value)
    #[arg(long, value_parser = NORM_METHODS, help_heading = "Normalization")]
    pub norm_method: Option<String>,
    /// default engine of every registration step (default: auto): preprocess, precoreg, coreg and normalize use it unless --preprocess-backend, --coregister-backend or --normalize-backend overrides it. Engine of the ants_syn normalization: 'wgpu' runs the registration on the GPU inside dbsloc (Vulkan, DirectX 12 or Metal), writes the same Composite.h5 files as ANTs and needs no ANTs binary for normalize; it is an error when no usable GPU exists. 'ants' runs the bundled ANTs processes (CPU). 'auto' uses the GPU when a hardware adapter passes the device self-test and ANTs otherwise. Software adapters are never used unless DBSLOC_GPU_ALLOW_SOFTWARE=1. Environment variable: DBSLOC_REG_BACKEND (same values); adapter selection: DBSLOC_GPU_BACKEND, DBSLOC_GPU_ADAPTER
    #[arg(long, value_parser = REG_BACKENDS, help_heading = "Normalization")]
    pub reg_backend: Option<String>,
    /// engine of preprocessing (bias field correction) and of the rigid alignment to the template (precoreg); overrides --reg-backend for those steps. Values as for --reg-backend. Environment variable: DBSLOC_PREPROCESS_BACKEND
    #[arg(long, value_parser = REG_BACKENDS, help_heading = "Normalization")]
    pub preprocess_backend: Option<String>,
    /// engine of coregistration (post-op image to the pre-op anchor); overrides --reg-backend for this step. Values as for --reg-backend. Environment variable: DBSLOC_COREGISTER_BACKEND
    #[arg(long, value_parser = REG_BACKENDS, help_heading = "Normalization")]
    pub coregister_backend: Option<String>,
    /// engine of normalization, the template space bridge and the point warp into template space; overrides --reg-backend for those steps. Values as for --reg-backend. Environment variable: DBSLOC_NORMALIZE_BACKEND
    #[arg(long, value_parser = REG_BACKENDS, help_heading = "Normalization")]
    pub normalize_backend: Option<String>,

    /// stderr progress and tool output: default (full), quiet (notes and warnings only), plain (ASCII lines)
    #[arg(
        long,
        default_value = "default",
        value_parser = LOG_MODES,
        help_heading = "Output"
    )]
    pub log: String,
    /// add total_seconds + stage_timings_sec to JSON
    #[arg(long, help_heading = "Output")]
    pub profile: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR", help_heading = "Output", value_hint = ValueHint::DirPath)]
    pub output_dir: Option<PathBuf>,
    /// directory of further Lead-DBS atlases for the 3D scene: a Lead-DBS installation folder or its atlases directory; every atlas folder in it is listed beside the bundled atlases when reconstruct or visualize writes the scene manifest (never copied); a relative path is taken from the current directory. Environment variable: DBSLOC_LEADDBS_ATLASES_DIR
    #[arg(long, value_name = "DIR", help_heading = "Output", value_hint = ValueHint::DirPath)]
    pub leaddbs_atlases_dir: Option<PathBuf>,
    /// post-op images the pipeline uses when a subject has both a post-op CT and a post-op MR: auto = CT unless the subject's uiprefs.mat chose MR; ct / mri force one and fail with E_MISS_POSTOP when that image is missing. Environment variable: DBSLOC_POSTOP_MODALITY
    #[arg(long, value_name = "MODALITY", value_parser = ["auto", "ct", "mri"], help_heading = "Reconstruction")]
    pub postop_modality: Option<String>,
    /// engine of the vat stage: auto = the GPU engine when it fits, else the CPU engine; wgpu = GPU only; cpu = the f64 CPU engine. Environment variable: DBSLOC_VAT_BACKEND
    #[arg(long, value_name = "BACKEND", value_parser = VAT_BACKENDS, help_heading = "Stimulation")]
    pub vat_backend: Option<String>,
    /// stimulation label the vat stage computes (repeatable or comma separated; default: every stimulation of the subject). Environment variable: DBSLOC_VAT_LABELS
    #[arg(
        long,
        value_name = "LABEL",
        value_delimiter = ',',
        help_heading = "Stimulation"
    )]
    pub vat_labels: Vec<String>,
    /// atlas set of the vat overlap statistics (default: the default atlas set, DISTAL Minimal). Environment variable: DBSLOC_VAT_STATS_ATLAS
    #[arg(long, value_name = "ATLAS", help_heading = "Stimulation")]
    pub vat_stats_atlas: Option<String>,
}

const DROP_FLAGS: [(&str, &[&str]); 7] = [
    ("no_normalize", &["normalize"]),
    ("no_brainshift", &["brainshift"]),
    ("no_reconstruct", &["reconstruct"]),
    ("no_orientation", &["orientation"]),
    ("no_vat", &["vat"]),
    ("no_visualize", &["visualize"]),
    ("no_coreg", &["precoreg", "coreg"]),
];

pub fn epilog() -> String {
    format!(
        "Default stage order (unless overridden by --stages or --no-* flags):\n  {}\n\nANTs: any --ants-<flag> with optional value is passed through to ANTs (e.g. --ants-float 1). For ANTs/ITK stage detail on stderr, add --ants-verbose 1 (or 2 if the binary supports it). Reserved: --ants-max-threads N sets only ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS (not forwarded to binaries).",
        stage_names().join(", ")
    )
}

impl RunArgs {
    fn drop_flag(&self, name: &str) -> bool {
        match name {
            "no_normalize" => self.no_normalize,
            "no_brainshift" => self.no_brainshift,
            "no_reconstruct" => self.no_reconstruct,
            "no_orientation" => self.no_orientation,
            "no_vat" => self.no_vat,
            "no_visualize" => self.no_visualize,
            "no_coreg" => self.no_coreg,
            _ => false,
        }
    }
}

pub fn planned_stages(
    stages_arg: Option<&str>,
    drops: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, Fail> {
    let known: Vec<String> = stage_names().iter().map(|s| (*s).to_owned()).collect();
    if let Some(arg) = stages_arg.filter(|s| !s.is_empty()) {
        let typed: Vec<&str> = arg
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let unknown: Vec<&str> = typed
            .iter()
            .copied()
            .filter(|s| Stage::parse(s).is_none())
            .collect();
        if !unknown.is_empty() {
            return Err(Fail::new(
                2,
                format!(
                    "dbsloc: error: unknown stage(s) {}; valid: {}",
                    unknown.join(", "),
                    known.join(", ")
                ),
            ));
        }
        let mut names: Vec<String> = Vec::with_capacity(typed.len());
        let mut repeated: Vec<String> = Vec::new();
        for st in typed.iter().filter_map(|s| Stage::parse(s)) {
            let name = st.cli_name().to_owned();
            if names.contains(&name) {
                if !repeated.contains(&name) {
                    repeated.push(name);
                }
            } else {
                names.push(name);
            }
        }
        if !repeated.is_empty() {
            return Err(Fail::new(
                2,
                format!(
                    "dbsloc: error: stage(s) listed more than once in --stages: {}",
                    repeated.join(", ")
                ),
            ));
        }
        return Ok(names);
    }
    let mut names = known;
    for (flag, drop) in DROP_FLAGS {
        if drops(flag) {
            names.retain(|s| !drop.contains(&s.as_str()));
        }
    }
    Ok(names)
}

const ANTS_ARGV_STAGES: [&str; 2] = ["coreg", "normalize"];
const ANTS_THREAD_STAGES: [&str; 6] = [
    "preprocess",
    "precoreg",
    "coreg",
    "normalize",
    "brainshift",
    "reconstruct",
];

fn warn_ants_flags_unused(apply: bool, planned: &[String], ants: &AntsParsed) {
    if ants.extra.is_empty() && ants.max_threads.is_none() {
        return;
    }
    let has_any = |set: &[&str]| planned.iter().any(|s| set.contains(&s.as_str()));
    let extra_used = ants.extra.is_empty() || has_any(&ANTS_ARGV_STAGES);
    let threads_used = ants.max_threads.is_none() || has_any(&ANTS_THREAD_STAGES);
    if apply && extra_used && threads_used {
        return;
    }
    err_line(
        "dbsloc: warning: some --ants-\u{2026} options have no effect on this run: they apply only when executing (not --plan); --ants-max-threads needs one of preprocess, precoreg, coreg, normalize, brainshift or reconstruct in the plan, the other --ants-\u{2026} options coreg or normalize.",
    );
}

fn build_options(
    args: &RunArgs,
    ants: &AntsParsed,
    stages: Vec<Stage>,
    subj_id: &str,
) -> Result<RunOptions, Fail> {
    let choices = options::resolve_choices(
        args.norm_isotropic_mm,
        args.norm_method.as_deref(),
        args.reg_backend.as_deref(),
        options::StepBackendFlags {
            preprocess: args.preprocess_backend.as_deref(),
            coregister: args.coregister_backend.as_deref(),
            normalize: args.normalize_backend.as_deref(),
        },
        ants.max_threads,
    )?;
    options::build(
        &RunInputs {
            dataset_root: &args.dataset_root,
            output_dir: args.output_dir.as_deref(),
            leaddbs_atlases_dir: args.leaddbs_atlases_dir.as_deref(),
            postop_modality: args
                .postop_modality
                .as_deref()
                .and_then(|m| PostopModality::parse(m).ok()),
            vat_backend: args
                .vat_backend
                .as_deref()
                .and_then(|b| VatBackend::parse(b).ok()),
            vat_labels: &args.vat_labels,
            vat_stats_atlas: args.vat_stats_atlas.as_deref(),
            subj_id,
            stages,
            template: args.template.as_ref(),
            template_t2: args.template_t2.as_ref(),
            no_norm_multispectral: args.no_norm_multispectral,
            recon_method: &args.recon_method,
            recon_bright_metal: args.recon_bright_metal,
            el_model: args.el_model.as_deref(),
            headless: args.headless || args.defer_review,
            no_checkreg: args.no_checkreg,
            no_scene3d: args.no_scene3d,
            plan: args.plan,
            overwrite_approved: args.overwrite_approved,
            fingerprint_locks: args.fingerprint_locks,
            no_index: args.no_index,
        },
        &choices,
        ants,
    )
}

fn log_mode(log: &str) -> (bool, bool) {
    match log {
        "quiet" => (false, false),
        "plain" => (true, true),
        _ => (true, false),
    }
}

pub fn run(args: &RunArgs, ants: &AntsParsed) -> CmdResult {
    let planned = planned_stages(args.stages.as_deref(), &|f| args.drop_flag(f))?;
    let stages: Vec<Stage> = planned.iter().filter_map(|s| Stage::parse(s)).collect();
    let mut opts = build_options(args, ants, stages, "")?;
    let subjects = batch::subject_ids(args, &opts)?;
    opts.subject = subjects.first().cloned().unwrap_or_default();
    let cancel = interrupt_flag();
    if subjects.len() > 1 {
        return batch::run_many(args, ants, &opts, &planned, &subjects, &cancel);
    }
    run_one(args, ants, &opts, &planned, &cancel)
}

fn run_one(
    args: &RunArgs,
    ants: &AntsParsed,
    opts: &RunOptions,
    planned: &[String],
    cancel: &AtomicBool,
) -> CmdResult {
    let plan = plan_run(opts).map_err(|e| bids_fail(&e))?;
    if cancel.load(Ordering::SeqCst) {
        err_line("dbsloc: interrupted");
        return Ok(130);
    }
    warn_ants_flags_unused(!opts.plan_only, planned, ants);

    let (do_progress, plain) = log_mode(&args.log);
    let friendly =
        do_progress.then(|| Friendly::new(&plan.subject.subj_id, &opts.bids_dir, planned, plain));
    let mut reporter = RunReporter::new(planned, friendly, do_progress);
    reporter.header();

    let (tx, rx) = mpsc::channel();
    let worker_failed = std::thread::scope(|s| {
        let handle = s.spawn(move || {
            let failed = run_pipeline_channel(opts, &tx, Some(cancel));
            drop(tx);
            failed
        });
        for ev in rx {
            reporter.handle(ev);
        }
        handle.join().is_err()
    });
    if worker_failed {
        reporter.record_failure("the pipeline thread panicked");
    }
    let interrupted = cancel.load(Ordering::SeqCst);

    let total = summary::total_seconds(&reporter.rows);
    reporter.footer(total, interrupted);
    let payload = summary::build(&Summary {
        plan: &plan.to_json(opts),
        apply: !opts.plan_only,
        planned,
        rows: &reporter.rows,
        profile: args.profile,
        interrupted,
    });
    out_json(&payload);
    if interrupted {
        err_line("dbsloc: interrupted");
        return Ok(130);
    }
    if summary::any_error(&reporter.rows) {
        return Ok(1);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn default_plan_is_every_stage() {
        let p = planned_stages(None, &none).unwrap_or_default();
        assert_eq!(p, stage_names());
    }

    #[test]
    fn no_flags_drop_stages() {
        let p = planned_stages(None, &|f| {
            matches!(f, "no_coreg" | "no_vat" | "no_brainshift")
        })
        .unwrap_or_default();
        assert!(!p
            .iter()
            .any(|s| matches!(s.as_str(), "coreg" | "precoreg" | "vat" | "brainshift")));
        assert!(p.contains(&"normalize".to_owned()));
    }

    #[test]
    fn stages_override_keeps_order_and_normalises_the_alias() {
        let p =
            planned_stages(Some(" reconstruct , coregister,,bids"), &|_| true).unwrap_or_default();
        assert_eq!(p, ["reconstruct", "coreg", "bids"]);
    }

    #[test]
    fn a_repeated_stage_exits_2() {
        let e = planned_stages(Some("coreg,bids,coregister"), &none).unwrap_err();
        assert_eq!(e.code, 2);
        let m = e.message.unwrap_or_default();
        assert!(m.contains("more than once") && m.contains("coreg"), "{m}");
    }

    #[test]
    fn empty_stages_value_falls_back_to_defaults() {
        let p = planned_stages(Some(""), &none).unwrap_or_default();
        assert_eq!(p, stage_names());
    }

    #[test]
    fn unknown_stage_exits_2_and_lists_valid_names() {
        let e = planned_stages(Some("bids,nonsense"), &none).unwrap_err();
        assert_eq!(e.code, 2);
        let m = e.message.unwrap_or_default();
        assert!(m.contains("nonsense") && m.contains("valid: bids"), "{m}");
    }

    #[test]
    fn epilog_lists_the_stages_and_the_ants_note() {
        let e = epilog();
        assert!(e.contains("bids, preprocess"));
        assert!(e.contains("--ants-max-threads N"));
    }
}
