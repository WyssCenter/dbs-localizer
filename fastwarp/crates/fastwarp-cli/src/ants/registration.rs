// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! `antsRegistration` arguments to a FastWarp stage plan.

use std::path::{Path, PathBuf};

use fastwarp_reg::linear::LinearKind;
use fastwarp_reg::pyramid::SigmaUnits;
use fastwarp_reg::transform::ants_io::{
    with_suffix, AFFINE_SUFFIX, COMPOSITE_SUFFIX, INVERSE_COMPOSITE_SUFFIX,
};
use serde_json::{json, Value};

use super::{
    call, check_image_name, dimensionality, interpolation_name, parse_bool, parse_f64,
    parse_interpolation, parse_u32, parse_u32_list, path_arg, refuse, same_path, tokenize,
    unsupported, version_text, Opt, OptSpec, Refusal, Takes,
};
use crate::post::Interp;
use crate::recipe::{kind_name, units_name, GPU_CC_RADIUS_MAX, GPU_MI_BINS};

const PROGRAM: &str = "antsRegistration";

const fn spec(short: Option<&'static str>, long: &'static str, takes: Takes) -> OptSpec {
    OptSpec { short, long, takes }
}

const SPECS: &[OptSpec] = &[
    spec(None, "--version", Takes::Nothing),
    spec(Some("-h"), "--help", Takes::Nothing),
    spec(Some("-d"), "--dimensionality", Takes::Required),
    spec(Some("-o"), "--output", Takes::Required),
    spec(Some("-j"), "--save-state", Takes::Required),
    spec(Some("-k"), "--restore-state", Takes::Required),
    spec(Some("-a"), "--write-composite-transform", Takes::Optional),
    spec(
        Some("-p"),
        "--print-similarity-measure-interval",
        Takes::Required,
    ),
    spec(None, "--write-interval-volumes", Takes::Required),
    spec(Some("-z"), "--collapse-output-transforms", Takes::Optional),
    spec(
        Some("-i"),
        "--initialize-transforms-per-stage",
        Takes::Optional,
    ),
    spec(Some("-n"), "--interpolation", Takes::Required),
    spec(Some("-g"), "--restrict-deformation", Takes::Required),
    spec(Some("-q"), "--initial-fixed-transform", Takes::Required),
    spec(Some("-r"), "--initial-moving-transform", Takes::Required),
    spec(Some("-m"), "--metric", Takes::Required),
    spec(Some("-t"), "--transform", Takes::Required),
    spec(Some("-c"), "--convergence", Takes::Required),
    spec(Some("-s"), "--smoothing-sigmas", Takes::Required),
    spec(Some("-f"), "--shrink-factors", Takes::Required),
    spec(Some("-u"), "--use-histogram-matching", Takes::Optional),
    spec(Some("-w"), "--winsorize-image-intensities", Takes::Required),
    spec(Some("-x"), "--masks", Takes::Required),
    spec(None, "--float", Takes::Optional),
    spec(None, "--minc", Takes::Optional),
    spec(None, "--random-seed", Takes::Required),
    spec(Some("-v"), "--verbose", Takes::Optional),
];

/// What an `antsRegistration` argument list asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum RegistrationCall {
    /// `-h` / `--help`: the text to print.
    Help(String),
    /// `--version`: the text to print.
    Version(String),
    /// A registration to run.
    Run(Box<RegistrationPlan>),
}

/// `--initial-moving-transform`.
#[derive(Clone, Debug, PartialEq)]
pub enum InitialArg {
    /// No `-r`: the identity, and the transform files are numbered from 0 (a SyN-only run writes
    /// `0Warp.nii.gz`).
    None,
    /// `[fixed,moving,1]`: the centres of intensity mass.
    CentreOfMass,
    /// `Identity`.
    Identity,
    /// `file` or `[file,useInverse]`: one affine transform.
    File { path: PathBuf, invert: bool },
}

/// A `Rigid[step]` or `Affine[step]` stage with one `MI` (Mattes) metric, regular sampling.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearStageSpec {
    pub kind: LinearKind,
    pub gradient_step: f64,
    pub iterations: Vec<u32>,
    pub convergence_threshold: f64,
    pub convergence_window: usize,
    pub shrink_factors: Vec<u32>,
    pub smoothing_sigmas: Vec<f64>,
    pub sigma_units: SigmaUnits,
    pub sampling_percentage: f64,
    /// The fixed-image mask of this stage (`--masks`).
    pub fixed_mask: Option<PathBuf>,
}

/// A `SyN[step,ufv,tfv]` stage with one or two `CC` metrics (channels).
#[derive(Clone, Debug, PartialEq)]
pub struct SynStageSpec {
    pub gradient_step: f64,
    pub update_field_variance: f64,
    pub total_field_variance: f64,
    pub cc_radius: usize,
    pub channel_weights: Vec<f64>,
    pub iterations: Vec<u32>,
    pub convergence_threshold: f64,
    pub convergence_window: usize,
    pub shrink_factors: Vec<u32>,
    pub smoothing_sigmas: Vec<f64>,
    pub sigma_units: SigmaUnits,
}

/// One stage, in command-line order.
#[derive(Clone, Debug, PartialEq)]
pub enum StageSpec {
    Linear(LinearStageSpec),
    Syn(SynStageSpec),
}

/// An `antsRegistration` call FastWarp runs: the images, the start, the stages and the outputs.
#[derive(Clone, Debug, PartialEq)]
pub struct RegistrationPlan {
    /// Fixed image of each channel (the first metric of the SyN stage first; a linear-only run
    /// has one channel).
    pub fixed: Vec<PathBuf>,
    /// Moving image of each channel.
    pub moving: Vec<PathBuf>,
    pub initial: InitialArg,
    /// The linear stages, then at most one SyN stage.
    pub stages: Vec<StageSpec>,
    /// `--winsorize-image-intensities [lower,upper]`; `None`: ANTs' plain rescale to [0, 1].
    pub winsorize: Option<(f64, f64)>,
    /// `--random-seed` (`None`: `ANTS_RANDOM_SEED`, else FastWarp's fixed default).
    pub seed: Option<u32>,
    /// The output prefix (taken as text, as ANTs does).
    pub prefix: PathBuf,
    /// `--output [prefix,warped,...]`: the moving image resampled onto the fixed grid.
    pub warped: Option<PathBuf>,
    /// `--output [prefix,warped,inverseWarped]`: the fixed image resampled onto the moving grid.
    pub inverse_warped: Option<PathBuf>,
    /// `--write-composite-transform 1`.
    pub composite: bool,
    /// `--interpolation` of the warped images.
    pub interpolation: Interp,
    /// `--float 1`: images and fields written as `float32` (else `float64`, as ANTs).
    pub float: bool,
    pub verbose: bool,
    /// Options accepted but without effect in FastWarp, to print as warnings.
    pub warnings: Vec<String>,
}

impl RegistrationPlan {
    /// Number of channels (1, or 2 with two SyN metrics).
    pub fn channels(&self) -> usize {
        self.fixed.len()
    }

    /// The linear stages, in order.
    pub fn linear_stages(&self) -> impl Iterator<Item = &LinearStageSpec> {
        self.stages.iter().filter_map(|s| match s {
            StageSpec::Linear(l) => Some(l),
            StageSpec::Syn(_) => None,
        })
    }

    /// The SyN stage, if any.
    pub fn syn_stage(&self) -> Option<&SynStageSpec> {
        self.stages.iter().find_map(|s| match s {
            StageSpec::Syn(s) => Some(s),
            StageSpec::Linear(_) => None,
        })
    }

    /// Whether ANTs writes `0GenericAffine.mat`: with an initial transform or a linear stage.
    pub fn has_affine_file(&self) -> bool {
        self.initial != InitialArg::None || self.linear_stages().next().is_some()
    }

    /// The number of the warp files: 1 after `0GenericAffine.mat`, else 0.
    pub fn warp_index(&self) -> usize {
        usize::from(self.has_affine_file())
    }

    /// The transform files ANTs 2.6.5 writes for this call, in the order FastWarp writes them.
    pub fn transform_files(&self) -> Vec<PathBuf> {
        if self.composite {
            return vec![
                with_suffix(&self.prefix, COMPOSITE_SUFFIX),
                with_suffix(&self.prefix, INVERSE_COMPOSITE_SUFFIX),
            ];
        }
        let mut files = Vec::new();
        if self.has_affine_file() {
            files.push(with_suffix(&self.prefix, AFFINE_SUFFIX));
        }
        if self.syn_stage().is_some() {
            let n = self.warp_index();
            files.push(with_suffix(&self.prefix, &format!("{n}Warp.nii.gz")));
            files.push(with_suffix(&self.prefix, &format!("{n}InverseWarp.nii.gz")));
        }
        files
    }

    /// Every file the call writes: the transforms, then the warped and inverse-warped images.
    pub fn output_files(&self) -> Vec<PathBuf> {
        let mut files = self.transform_files();
        files.extend(self.warped.clone());
        files.extend(self.inverse_warped.clone());
        files
    }

    /// The recipe (stage-list form, `fastwarp register --recipe`) of the call. A start from a
    /// file is given as `{"path": ..., "invert": ...}`; fixed masks are not part of a recipe
    /// (see [`RegistrationPlan::to_json`]). Without `--random-seed` the stages keep the default
    /// seed.
    pub fn recipe_json(&self) -> Value {
        let initial = match &self.initial {
            InitialArg::CentreOfMass => json!("com"),
            InitialArg::None | InitialArg::Identity => json!("identity"),
            InitialArg::File { path, invert } => {
                json!({"path": path.to_string_lossy(), "invert": invert})
            }
        };
        let stages: Vec<Value> = self
            .stages
            .iter()
            .map(|s| match s {
                StageSpec::Linear(l) => {
                    let mut v = json!({
                        "transform": kind_name(l.kind),
                        "gradient_step": l.gradient_step,
                        "iterations": l.iterations,
                        "convergence_threshold": l.convergence_threshold,
                        "convergence_window": l.convergence_window,
                        "shrink_factors": l.shrink_factors,
                        "smoothing_sigmas": l.smoothing_sigmas,
                        "sigma_units": units_name(l.sigma_units),
                        "bins": GPU_MI_BINS,
                        "sampling_percentage": l.sampling_percentage,
                    });
                    if let Some(seed) = self.seed {
                        v["seed"] = json!(seed);
                    }
                    v
                }
                StageSpec::Syn(s) => json!({
                    "transform": "syn",
                    "gradient_step": s.gradient_step,
                    "update_field_variance": s.update_field_variance,
                    "total_field_variance": s.total_field_variance,
                    "cc_radius": s.cc_radius,
                    "channel_weights": s.channel_weights,
                    "iterations": s.iterations,
                    "convergence_threshold": s.convergence_threshold,
                    "convergence_window": s.convergence_window,
                    "shrink_factors": s.shrink_factors,
                    "smoothing_sigmas": s.smoothing_sigmas,
                    "sigma_units": units_name(s.sigma_units),
                }),
            })
            .collect();
        let winsorize = match self.winsorize {
            Some((lo, hi)) => json!([lo, hi]),
            None => Value::Null,
        };
        json!({"initial": initial, "stages": stages, "winsorize": winsorize})
    }

    /// The whole plan as JSON: the images, the recipe, the masks, the outputs and the options.
    pub fn to_json(&self) -> Value {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let initial = match &self.initial {
            InitialArg::None => Value::Null,
            InitialArg::CentreOfMass => json!("com"),
            InitialArg::Identity => json!("identity"),
            InitialArg::File { path, invert } => json!({"path": text(path), "invert": invert}),
        };
        json!({
            "program": PROGRAM,
            "fixed": self.fixed.iter().map(|p| text(p)).collect::<Vec<_>>(),
            "moving": self.moving.iter().map(|p| text(p)).collect::<Vec<_>>(),
            "initial": initial,
            "recipe": self.recipe_json(),
            "fixed_masks": self
                .linear_stages()
                .map(|l| l.fixed_mask.as_deref().map(text))
                .collect::<Vec<_>>(),
            "prefix": text(&self.prefix),
            "warped": self.warped.as_deref().map(text),
            "inverse_warped": self.inverse_warped.as_deref().map(text),
            "composite": self.composite,
            "files": self.output_files().iter().map(|p| text(p)).collect::<Vec<_>>(),
            "interpolation": interpolation_name(self.interpolation),
            "float": self.float,
            "seed": self.seed,
            "verbose": self.verbose,
            "warnings": self.warnings,
        })
    }
}

/// The help text of `fastwarp ants-registration`.
pub(crate) fn help_text() -> String {
    format!(
        "{}\
Usage: fastwarp ants-registration [antsRegistration options]

Runs the subset of antsRegistration (ANTs 2.6.5) that FastWarp supports on the GPU and writes the
files antsRegistration writes. Options that would change the result and are not supported are
refused with exit status 2 before any work. See docs/cli.md.

Supported:
  -d 3   -o PREFIX | [PREFIX,warped.nii.gz,inverseWarped.nii.gz]
  -r [fixed,moving,1] | Identity | file.mat | [file.mat,1]
  stages, any number of Rigid / Affine, then at most one SyN:
    -t Rigid[step] | Affine[step]   -m MI[fixed,moving,w,32,Regular,percentage]
    -t SyN[step,ufv,tfv]            -m CC[fixed,moving,w,radius 1..8] (one or two)
    -c [MxN,threshold,window]  -f MxN  -s MxN[vox|mm]
  -x [fixedMask,NULL] (Rigid / Affine stages)   -w [lower,upper]   -u 0
  -a 0|1   -z 1   -i 0   -n Linear|NearestNeighbor|LanczosWindowedSinc
  --float 0|1   --random-seed N   -v 0|1   --version   -h, --help
",
        version_text(PROGRAM)
    )
}

struct Group {
    t: Vec<Opt>,
    m: Vec<Opt>,
    c: Vec<Opt>,
    f: Vec<Opt>,
    s: Vec<Opt>,
}

/// ANTs' assignment of the stage options: the k-th `--transform`, `--convergence`,
/// `--shrink-factors` and `--smoothing-sigmas` belong to stage k, and a `--metric` starts the
/// metrics of the next stage unless the option just before it is a `--metric` too.
fn group_stages(opts: &[Opt]) -> Result<Vec<Group>, Refusal> {
    let (mut t, mut c, mut f, mut s) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut metrics: Vec<Vec<Opt>> = Vec::new();
    let mut previous_was_metric = false;
    for o in opts {
        match o.key {
            "--metric" => match metrics.last_mut() {
                Some(last) if previous_was_metric => last.push(o.clone()),
                _ => metrics.push(vec![o.clone()]),
            },
            "--transform" => t.push(o.clone()),
            "--convergence" => c.push(o.clone()),
            "--shrink-factors" => f.push(o.clone()),
            "--smoothing-sigmas" => s.push(o.clone()),
            _ => {}
        }
        previous_was_metric = o.key == "--metric";
    }
    if metrics.is_empty() && t.is_empty() && c.is_empty() && f.is_empty() && s.is_empty() {
        return refuse(
            "no stage given: each stage needs --transform, --metric, --convergence, \
             --shrink-factors and --smoothing-sigmas",
        );
    }
    if metrics.len() != t.len() {
        return refuse(format!(
            "the number of stages does not match up with the metrics: {} --transform and {} \
             metric stages (a --metric starts a new stage unless it follows another --metric)",
            t.len(),
            metrics.len()
        ));
    }
    for (list, name) in [
        (&c, "--convergence"),
        (&f, "--shrink-factors"),
        (&s, "--smoothing-sigmas"),
    ] {
        if list.len() != t.len() {
            return refuse(format!(
                "{name} is given {} times for {} --transform: give it once per stage",
                list.len(),
                t.len()
            ));
        }
    }
    Ok((0..t.len())
        .map(|k| Group {
            t: vec![t[k].clone()],
            m: metrics[k].clone(),
            c: vec![c[k].clone()],
            f: vec![f[k].clone()],
            s: vec![s[k].clone()],
        })
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransformKind {
    Rigid,
    Affine,
    Syn,
}

impl TransformKind {
    fn name(self) -> &'static str {
        match self {
            TransformKind::Rigid => "Rigid",
            TransformKind::Affine => "Affine",
            TransformKind::Syn => "SyN",
        }
    }
}

struct TransformArg {
    kind: TransformKind,
    params: Vec<f64>,
}

fn parse_transform(o: &Opt) -> Result<TransformArg, Refusal> {
    let v = o.value()?;
    let c = call(v)?;
    let name = if c.bracketed {
        c.name.clone()
    } else {
        v.to_string()
    };
    let kind =
        match name.to_ascii_lowercase().as_str() {
            "rigid" => TransformKind::Rigid,
            "affine" => TransformKind::Affine,
            "syn" => TransformKind::Syn,
            "translation" | "similarity" => return unsupported(
                format!("transform {name}"),
                "FastWarp has no Translation or Similarity transform; it runs Rigid, Affine and \
                 SyN.",
            ),
            "compositeaffine"
            | "bspline"
            | "gaussiandisplacementfield"
            | "bsplinedisplacementfield"
            | "timevaryingvelocityfield"
            | "timevaryingbsplinevelocityfield"
            | "bsplinesyn"
            | "exponential"
            | "bsplineexponential" => {
                return unsupported(
                    format!("transform {name}"),
                    "FastWarp runs Rigid, Affine and SyN.",
                )
            }
            _ => return refuse(format!("unknown transform {v}")),
        };
    let what = format!("{} {v}", o.spelled);
    let args = if c.bracketed {
        c.args.clone()
    } else {
        Vec::new()
    };
    let max = if kind == TransformKind::Syn { 3 } else { 1 };
    if args.is_empty() || args.len() > max {
        return refuse(match kind {
            TransformKind::Syn => format!(
                "{what}: expected SyN[gradientStep,<updateFieldVariance=3>,\
                 <totalFieldVariance=0>]"
            ),
            k => format!("{what}: expected {}[gradientStep]", k.name()),
        });
    }
    let params = args
        .iter()
        .map(|a| parse_f64(a, &what))
        .collect::<Result<Vec<_>, _>>()?;
    if params[0] <= 0.0 {
        return refuse(format!("{what}: the gradient step must be positive"));
    }
    if params[1..].iter().any(|p| *p < 0.0) {
        return refuse(format!("{what}: the field variances must not be negative"));
    }
    Ok(TransformArg { kind, params })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetricKind {
    Mi,
    Cc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    Dense,
    Regular,
    Random,
}

struct MetricArg {
    kind: MetricKind,
    name: String,
    fixed: PathBuf,
    moving: PathBuf,
    weight: f64,
    param: u32,
    strategy: Strategy,
    percentage: Option<f64>,
}

fn parse_metric(o: &Opt) -> Result<MetricArg, Refusal> {
    let v = o.value()?;
    let c = call(v)?;
    let name = if c.bracketed {
        c.name.clone()
    } else {
        v.to_string()
    };
    let kind = match name.to_ascii_lowercase().as_str() {
        "mi" | "mattes" => MetricKind::Mi,
        "cc" => MetricKind::Cc,
        "meansquares" | "demons" | "gc" => {
            return unsupported(
                format!("metric {name}"),
                "FastWarp uses MI (Mattes) in Rigid and Affine stages and CC in the SyN stage.",
            )
        }
        "icp" | "pse" | "jhct" | "igdm" => {
            return unsupported(
                format!("metric {name}"),
                "FastWarp has no point-set metrics.",
            )
        }
        _ => return refuse(format!("unknown metric {v}")),
    };
    let what = format!("{} {v}", o.spelled);
    let a = &c.args;
    if !c.bracketed || a.len() < 4 || a.len() > 7 {
        return refuse(format!(
            "{what}: expected {name}[fixedImage,movingImage,metricWeight,{},\
             <samplingStrategy>,<samplingPercentage>,<useGradientFilter>]",
            if kind == MetricKind::Mi {
                "numberOfBins"
            } else {
                "radius"
            }
        ));
    }
    let fixed = path_arg(&a[0], &what)?;
    let moving = path_arg(&a[1], &what)?;
    check_image_name(&fixed)?;
    check_image_name(&moving)?;
    let weight = parse_f64(&a[2], &what)?;
    if weight < 0.0 {
        return refuse(format!("{what}: the metric weight must not be negative"));
    }
    let param = parse_u32(&a[3], &what)?;
    let strategy = match a.get(4).map(|s| s.to_ascii_lowercase()).as_deref() {
        None | Some("none") => Strategy::Dense,
        Some("regular") => Strategy::Regular,
        Some("random") => Strategy::Random,
        Some(other) => {
            return refuse(format!(
                "{what}: the sampling strategy is None, Regular or Random, not {other:?}"
            ))
        }
    };
    let percentage = a.get(5).map(|p| parse_f64(p, &what)).transpose()?;
    if let Some(p) = percentage {
        if !(p > 0.0 && p <= 1.0) {
            return refuse(format!(
                "{what}: the sampling percentage must be in (0, 1], got {p}"
            ));
        }
    }
    let gradient_filter = match a.get(6).map(|s| s.to_ascii_lowercase()).as_deref() {
        None | Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(other) => {
            return refuse(format!(
                "{what}: useGradientFilter is 0 or 1, not {other:?}"
            ))
        }
    };
    if gradient_filter {
        return unsupported(
            format!("metric {name} with useGradientFilter 1"),
            "FastWarp does not smooth the metric gradient.",
        );
    }
    Ok(MetricArg {
        kind,
        name,
        fixed,
        moving,
        weight,
        param,
        strategy,
        percentage,
    })
}

struct Schedule {
    iterations: Vec<u32>,
    threshold: f64,
    window: usize,
    shrink: Vec<u32>,
    sigmas: Vec<f64>,
    units: SigmaUnits,
}

fn parse_schedule(g: &Group, k: usize) -> Result<Schedule, Refusal> {
    let c = &g.c[0];
    let cv = c.value()?;
    let what = format!("{} {cv}", c.spelled);
    let cc = call(cv)?;
    let (iterations, threshold, window) = if cc.bracketed {
        if !cc.name.is_empty() || cc.args.is_empty() || cc.args.len() > 3 {
            return refuse(format!(
                "{what}: expected MxNxO or [MxNxO,<convergenceThreshold=1e-6>,\
                 <convergenceWindowSize=10>]"
            ));
        }
        let iterations = parse_u32_list(&cc.args[0], &what)?;
        let threshold = cc
            .args
            .get(1)
            .map(|t| parse_f64(t, &what))
            .transpose()?
            .unwrap_or(1e-6);
        let window = cc
            .args
            .get(2)
            .map(|w| parse_u32(w, &what))
            .transpose()?
            .unwrap_or(10);
        (iterations, threshold, window as usize)
    } else {
        (parse_u32_list(cv, &what)?, 1e-6, 10)
    };
    if window < 2 {
        return refuse(format!("{what}: the convergence window must be at least 2"));
    }
    let f = &g.f[0];
    let fv = f.value()?;
    let shrink = parse_u32_list(fv, &format!("{} {fv}", f.spelled))?;
    if shrink.contains(&0) {
        return refuse(format!(
            "{} {fv}: shrink factors must be at least 1",
            f.spelled
        ));
    }
    let s = &g.s[0];
    let sv = s.value()?;
    let swhat = format!("{} {sv}", s.spelled);
    let lower = sv.to_ascii_lowercase();
    let (numbers, units) = if let Some(n) = lower.strip_suffix("vox") {
        (n, SigmaUnits::Voxels)
    } else if let Some(n) = lower.strip_suffix("mm") {
        (n, SigmaUnits::Millimetres)
    } else {
        (lower.as_str(), SigmaUnits::Voxels)
    };
    let sigmas = numbers
        .split('x')
        .map(|t| parse_f64(t, &swhat))
        .collect::<Result<Vec<_>, _>>()?;
    if sigmas.iter().any(|v| *v < 0.0) {
        return refuse(format!("{swhat}: smoothing sigmas must not be negative"));
    }
    if iterations.len() != shrink.len() || iterations.len() != sigmas.len() {
        return refuse(format!(
            "stage {k}: --convergence gives {} levels, --shrink-factors {} and \
             --smoothing-sigmas {}: give one value per level",
            iterations.len(),
            shrink.len(),
            sigmas.len()
        ));
    }
    Ok(Schedule {
        iterations,
        threshold,
        window,
        shrink,
        sigmas,
        units,
    })
}

struct ParsedStage {
    transform: TransformArg,
    metrics: Vec<MetricArg>,
    schedule: Schedule,
}

fn check_linear_metrics(
    k: usize,
    kind: TransformKind,
    metrics: &[MetricArg],
) -> Result<(), Refusal> {
    if metrics.len() > 1 {
        return unsupported(
            format!(
                "stage {k}: {} metrics in the {} stage",
                metrics.len(),
                kind.name()
            ),
            "FastWarp's Rigid and Affine stages use one MI metric.",
        );
    }
    let m = &metrics[0];
    if m.kind == MetricKind::Cc {
        return unsupported(
            format!("metric {} in a {} stage", m.name, kind.name()),
            "FastWarp's Rigid and Affine stages use MI (Mattes).",
        );
    }
    if m.param as usize != GPU_MI_BINS {
        return unsupported(
            format!("metric {} with {} bins", m.name, m.param),
            format!("the GPU kernels use {GPU_MI_BINS} histogram bins."),
        );
    }
    match m.strategy {
        Strategy::Regular => Ok(()),
        Strategy::Dense => unsupported(
            format!(
                "metric {} with dense sampling (no sampling strategy, or None)",
                m.name
            ),
            "FastWarp samples MI on a regular grid; add Regular and a percentage, for example \
             MI[fixed,moving,1,32,Regular,0.25].",
        ),
        Strategy::Random => unsupported(
            format!("metric {} with Random sampling", m.name),
            "FastWarp samples MI on a regular grid; use Regular.",
        ),
    }
}

fn check_syn_metrics(k: usize, metrics: &[MetricArg]) -> Result<(), Refusal> {
    for m in metrics {
        if m.kind == MetricKind::Mi {
            return unsupported(
                format!("metric {} in a SyN stage", m.name),
                "FastWarp's SyN stage uses CC.",
            );
        }
    }
    if metrics.len() > 2 {
        return unsupported(
            format!("stage {k}: {} metrics in the SyN stage", metrics.len()),
            "FastWarp's SyN stage takes one or two CC metrics (channels).",
        );
    }
    for m in metrics {
        if m.param == 0 || m.param as usize > GPU_CC_RADIUS_MAX {
            return unsupported(
                format!("metric CC with radius {}", m.param),
                format!("the GPU kernels support radius 1 to {GPU_CC_RADIUS_MAX}."),
            );
        }
        let strategy = match m.strategy {
            Strategy::Dense => continue,
            Strategy::Regular => "Regular",
            Strategy::Random => "Random",
        };
        return unsupported(
            format!("metric CC with {strategy} sampling"),
            "FastWarp's CC is dense; leave the sampling strategy out or give None.",
        );
    }
    if let [a, b] = metrics {
        if a.param != b.param {
            return unsupported(
                format!("stage {k}: CC radii {} and {}", a.param, b.param),
                "FastWarp's SyN stage uses one radius for both channels.",
            );
        }
    }
    Ok(())
}

fn single<'a>(opts: &'a [Opt], key: &str) -> Result<Option<&'a Opt>, Refusal> {
    let found: Vec<&Opt> = opts.iter().filter(|o| o.key == key).collect();
    match found.as_slice() {
        [] => Ok(None),
        [o] => Ok(Some(o)),
        [o, ..] => refuse(format!(
            "{} is given {} times; give it once",
            o.spelled,
            found.len()
        )),
    }
}

fn is_null(text: &str) -> bool {
    matches!(
        text.to_ascii_lowercase().as_str(),
        "" | "null" | "na" | "none"
    )
}

fn parse_masks(opts: &[Opt], stages: usize) -> Result<Vec<Option<PathBuf>>, Refusal> {
    let mut masks = Vec::new();
    for o in opts.iter().filter(|o| o.key == "--masks") {
        let v = o.value()?;
        let c = call(v)?;
        let (fixed, moving) = if c.bracketed {
            if !c.name.is_empty() || c.args.len() > 2 {
                return refuse(format!(
                    "{} {v}: expected [fixedImageMask,movingImageMask]",
                    o.spelled
                ));
            }
            (
                c.args.first().cloned().unwrap_or_default(),
                c.args.get(1).cloned().unwrap_or_default(),
            )
        } else {
            (v.to_string(), String::new())
        };
        if !is_null(&moving) {
            return unsupported(
                "a moving-image mask in --masks",
                "FastWarp has no moving-image mask.",
            );
        }
        let fixed = if is_null(&fixed) {
            None
        } else {
            let p = PathBuf::from(fixed);
            check_image_name(&p)?;
            Some(p)
        };
        masks.push(fixed);
    }
    match masks.len() {
        0 => Ok(vec![None; stages]),
        1 => Ok(vec![masks[0].clone(); stages]),
        n if n == stages => Ok(masks),
        n => refuse(format!(
            "--masks is given {n} times for {stages} stages: give it once or once per stage"
        )),
    }
}

fn parse_initial(o: &Opt) -> Result<InitialArg, Refusal> {
    let v = o.value()?;
    let c = call(v)?;
    let what = format!("{} {v}", o.spelled);
    if c.bracketed && !c.name.is_empty() {
        return refuse(format!(
            "{what}: expected a transform file, [file,useInverse], [fixed,moving,feature] or \
             Identity"
        ));
    }
    let file = |path: &str, invert: bool| -> Result<InitialArg, Refusal> {
        let p = path_arg(path, &what)?;
        let name = p.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".nii") || name.ends_with(".nii.gz") {
            return unsupported(
                format!("{} with a displacement field", o.spelled),
                "FastWarp starts from one affine transform (.mat, .txt).",
            );
        }
        Ok(InitialArg::File { path: p, invert })
    };
    if !c.bracketed {
        if v.eq_ignore_ascii_case("identity") {
            return Ok(InitialArg::Identity);
        }
        return file(v, false);
    }
    match c.args.as_slice() {
        [path] => file(path, false),
        [path, flag] => {
            let invert = match flag.to_ascii_lowercase().as_str() {
                "0" | "false" => false,
                "1" | "true" => true,
                other => return refuse(format!("{what}: useInverse is 0 or 1, not {other:?}")),
            };
            file(path, invert)
        }
        [_, _, feature] => match feature.as_str() {
            "1" => Ok(InitialArg::CentreOfMass),
            "0" | "2" => unsupported(
                what,
                "FastWarp initialises by the centres of intensity mass (feature 1) only.",
            ),
            other => refuse(format!(
                "{what}: the initialization feature is 0, 1 or 2, not {other:?}"
            )),
        },
        _ => refuse(format!(
            "{what}: expected a transform file, [file,useInverse], [fixed,moving,feature] or \
             Identity"
        )),
    }
}

fn parse_output(o: &Opt) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>), Refusal> {
    let v = o.value()?;
    let c = call(v)?;
    let what = format!("{} {v}", o.spelled);
    if !c.bracketed {
        return Ok((path_arg(v, &what)?, None, None));
    }
    if !c.name.is_empty() || c.args.is_empty() || c.args.len() > 3 {
        return refuse(format!(
            "{what}: expected PREFIX or [PREFIX,<warpedImage>,<inverseWarpedImage>]"
        ));
    }
    let prefix = path_arg(&c.args[0], &what)?;
    let image = |i: usize| -> Result<Option<PathBuf>, Refusal> {
        match c.args.get(i).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => {
                let p = PathBuf::from(s);
                check_image_name(&p)?;
                Ok(Some(p))
            }
        }
    };
    Ok((prefix, image(1)?, image(2)?))
}

fn check_flag_value(o: Option<&Opt>, ok: u32, why: &str) -> Result<(), Refusal> {
    if let Some(o) = o {
        let what = format!("{} {}", o.spelled, o.value()?);
        if parse_u32(o.value()?, &what)? != ok {
            return unsupported(o.shown(), why);
        }
    }
    Ok(())
}

/// Parses the arguments of `antsRegistration` (without the program name). Pure: reads no file
/// and no environment variable; image names are checked by their extension only. Resolve the
/// seed with [`super::resolve_seed`].
///
/// Checked later, when the files are read (`fastwarp ants-registration` exits 3 for a file that
/// cannot be read, 2 for the rest, all before the GPU is opened): that the images and masks exist
/// and are 3-D NIfTI, that the fixed images of two SyN channels share one grid, that a `-r` file
/// holds one invertible affine (not a displacement field or a composite of several transforms),
/// that the output folders exist. The GPU capacity check follows (exit 5).
pub fn parse_registration<S: AsRef<str>>(args: &[S]) -> Result<RegistrationCall, Refusal> {
    let args: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    let opts = tokenize(&args, SPECS)?;
    if opts.iter().any(|o| o.key == "--help") {
        return Ok(RegistrationCall::Help(help_text()));
    }
    if opts.iter().any(|o| o.key == "--version") {
        return Ok(RegistrationCall::Version(version_text(PROGRAM)));
    }
    for key in [
        "--dimensionality",
        "--output",
        "--write-composite-transform",
        "--print-similarity-measure-interval",
        "--write-interval-volumes",
        "--collapse-output-transforms",
        "--initialize-transforms-per-stage",
        "--interpolation",
        "--use-histogram-matching",
        "--winsorize-image-intensities",
        "--float",
        "--minc",
        "--random-seed",
        "--verbose",
    ] {
        single(&opts, key)?;
    }
    if let Some(o) = single(&opts, "--dimensionality")? {
        dimensionality(o)?;
    }
    if let Some(o) = opts.iter().find(|o| o.key == "--save-state") {
        return unsupported(
            &o.spelled,
            "FastWarp cannot save or restore the state of a registration.",
        );
    }
    if let Some(o) = opts.iter().find(|o| o.key == "--restore-state") {
        return unsupported(
            &o.spelled,
            "FastWarp cannot save or restore the state of a registration.",
        );
    }
    if let Some(o) = opts.iter().find(|o| o.key == "--initial-fixed-transform") {
        return unsupported(&o.spelled, "FastWarp has no initial fixed transform.");
    }
    check_flag_value(
        single(&opts, "--print-similarity-measure-interval")?,
        0,
        "FastWarp does not compute the full-resolution metric during the registration.",
    )?;
    check_flag_value(
        single(&opts, "--write-interval-volumes")?,
        0,
        "FastWarp does not write intermediate volumes.",
    )?;
    if let Some(o) = single(&opts, "--collapse-output-transforms")? {
        if !parse_bool(o, true)? {
            return unsupported(
                o.shown(),
                "FastWarp writes collapsed transforms only (one affine and one warp per \
                 direction).",
            );
        }
    }
    if let Some(o) = single(&opts, "--initialize-transforms-per-stage")? {
        if parse_bool(o, true)? {
            return unsupported(
                o.shown(),
                "each linear stage starts from the identity centred on the previous result, as \
                 with 0.",
            );
        }
    }
    for o in opts.iter().filter(|o| o.key == "--restrict-deformation") {
        let v = o.value()?;
        let weights = v
            .split('x')
            .map(|t| parse_f64(t, &o.shown()))
            .collect::<Result<Vec<_>, _>>()?;
        if weights.iter().any(|w| *w != 1.0) {
            return unsupported(o.shown(), "FastWarp cannot restrict transform components.");
        }
    }
    if let Some(o) = single(&opts, "--use-histogram-matching")? {
        match o.value {
            None => return refuse(format!("{} needs a value, 0 or 1", o.spelled)),
            Some(_) => {
                if parse_bool(o, true)? {
                    return unsupported(o.shown(), "FastWarp has no histogram matching.");
                }
            }
        }
    }
    if let Some(o) = single(&opts, "--minc")? {
        if parse_bool(o, true)? {
            return unsupported(o.shown(), "FastWarp writes ITK transform files.");
        }
    }
    let interpolation = match single(&opts, "--interpolation")? {
        Some(o) => parse_interpolation(o.value()?)?,
        None => Interp::Linear,
    };
    let composite = match single(&opts, "--write-composite-transform")? {
        Some(o) => parse_bool(o, true)?,
        None => false,
    };
    let float = match single(&opts, "--float")? {
        Some(o) => parse_bool(o, true)?,
        None => false,
    };
    let verbose = match single(&opts, "--verbose")? {
        Some(o) => parse_bool(o, true)?,
        None => false,
    };
    let seed = match single(&opts, "--random-seed")? {
        Some(o) => Some(parse_u32(o.value()?, &o.spelled)?),
        None => None,
    };
    let winsorize = match single(&opts, "--winsorize-image-intensities")? {
        None => None,
        Some(o) => {
            let v = o.value()?;
            let c = call(v)?;
            let parts: Vec<String> = if c.bracketed {
                c.args.clone()
            } else {
                v.split(',').map(str::to_string).collect()
            };
            let what = o.shown();
            if (c.bracketed && !c.name.is_empty()) || parts.len() != 2 {
                return refuse(format!("{what}: expected [lowerQuantile,upperQuantile]"));
            }
            let lo = parse_f64(&parts[0], &what)?;
            let hi = parse_f64(&parts[1], &what)?;
            if !(0.0 <= lo && lo < hi && hi <= 1.0) {
                return refuse(format!(
                    "{what}: expected quantiles with 0 <= lower < upper <= 1"
                ));
            }
            Some((lo, hi))
        }
    };

    let groups = group_stages(&opts)?;
    let mut parsed = Vec::with_capacity(groups.len());
    for (i, g) in groups.iter().enumerate() {
        let k = i + 1;
        let transform = parse_transform(&g.t[0])?;
        let metrics =
            g.m.iter()
                .map(parse_metric)
                .collect::<Result<Vec<_>, _>>()?;
        match transform.kind {
            TransformKind::Syn => check_syn_metrics(k, &metrics)?,
            kind => check_linear_metrics(k, kind, &metrics)?,
        }
        let schedule = parse_schedule(g, k)?;
        parsed.push(ParsedStage {
            transform,
            metrics,
            schedule,
        });
    }
    let kinds: Vec<TransformKind> = parsed.iter().map(|p| p.transform.kind).collect();
    let syn_at = kinds.iter().position(|k| *k == TransformKind::Syn);
    if let Some(at) = syn_at {
        if at + 1 != kinds.len() {
            let order: Vec<&str> = kinds.iter().map(|k| k.name()).collect();
            return unsupported(
                format!("stage order {}", order.join(", ")),
                "FastWarp runs the linear stages first and at most one SyN stage, last.",
            );
        }
    }
    let channel_stage = syn_at.unwrap_or(0);
    let (fixed, moving): (Vec<PathBuf>, Vec<PathBuf>) = parsed[channel_stage]
        .metrics
        .iter()
        .map(|m| (m.fixed.clone(), m.moving.clone()))
        .unzip();
    let source = if syn_at.is_some() {
        "the first SyN metric"
    } else {
        "the first stage"
    };
    for (i, p) in parsed.iter().enumerate() {
        let m = &p.metrics[0];
        if p.transform.kind != TransformKind::Syn
            && !(same_path(&m.fixed, &fixed[0]) && same_path(&m.moving, &moving[0]))
        {
            return unsupported(
                format!(
                    "stage {} with the images {}, {}",
                    i + 1,
                    m.fixed.display(),
                    m.moving.display()
                ),
                format!(
                    "FastWarp's linear stages register the images of {source} ({}, {}).",
                    fixed[0].display(),
                    moving[0].display()
                ),
            );
        }
    }

    let initial_opts: Vec<&Opt> = opts
        .iter()
        .filter(|o| o.key == "--initial-moving-transform")
        .collect();
    let initial = match initial_opts.as_slice() {
        [] => InitialArg::None,
        [o] => {
            let initial = parse_initial(o)?;
            if initial == InitialArg::CentreOfMass {
                let c = call(o.value()?)?;
                let (f, m) = (Path::new(&c.args[0]), Path::new(&c.args[1]));
                if !(same_path(f, &fixed[0]) && same_path(m, &moving[0])) {
                    return unsupported(
                        format!("{} with images other than the first metric's", o.shown()),
                        "FastWarp computes the centres of mass on the images of the first \
                         metric.",
                    );
                }
            }
            initial
        }
        [o, ..] => {
            return unsupported(
                format!("{} given {} times", o.spelled, initial_opts.len()),
                "FastWarp starts from one initial transform.",
            )
        }
    };

    let masks = parse_masks(&opts, parsed.len())?;
    let mut stages = Vec::with_capacity(parsed.len());
    for (p, mask) in parsed.into_iter().zip(masks) {
        let s = p.schedule;
        match p.transform.kind {
            TransformKind::Syn => {
                if mask.is_some() {
                    return unsupported(
                        "a fixed-image mask on the SyN stage (--masks)",
                        "FastWarp's SyN stage has no mask; give NULL for that stage.",
                    );
                }
                let t = &p.transform.params;
                stages.push(StageSpec::Syn(SynStageSpec {
                    gradient_step: t[0],
                    update_field_variance: t.get(1).copied().unwrap_or(3.0),
                    total_field_variance: t.get(2).copied().unwrap_or(0.0),
                    cc_radius: p.metrics[0].param as usize,
                    channel_weights: p.metrics.iter().map(|m| m.weight).collect(),
                    iterations: s.iterations,
                    convergence_threshold: s.threshold,
                    convergence_window: s.window,
                    shrink_factors: s.shrink,
                    smoothing_sigmas: s.sigmas,
                    sigma_units: s.units,
                }));
            }
            kind => {
                let m = &p.metrics[0];
                stages.push(StageSpec::Linear(LinearStageSpec {
                    kind: if kind == TransformKind::Rigid {
                        LinearKind::Rigid
                    } else {
                        LinearKind::Affine
                    },
                    gradient_step: p.transform.params[0],
                    iterations: s.iterations,
                    convergence_threshold: s.threshold,
                    convergence_window: s.window,
                    shrink_factors: s.shrink,
                    smoothing_sigmas: s.sigmas,
                    sigma_units: s.units,
                    sampling_percentage: m.percentage.unwrap_or(1.0),
                    fixed_mask: mask,
                }));
            }
        }
    }
    if let Some(StageSpec::Syn(syn)) = stages.last() {
        if syn.channel_weights.iter().sum::<f64>() <= 0.0 {
            return refuse("the weights of the SyN metrics must not all be 0");
        }
    }

    let Some(out) = single(&opts, "--output")? else {
        return refuse("--output is required");
    };
    let (prefix, warped, inverse_warped) = parse_output(out)?;
    Ok(RegistrationCall::Run(Box::new(RegistrationPlan {
        fixed,
        moving,
        initial,
        stages,
        winsorize,
        seed,
        prefix,
        warped,
        inverse_warped,
        composite,
        interpolation,
        float,
        verbose,
        warnings: Vec::new(),
    })))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::recipe::Recipe;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn plan(s: &str) -> RegistrationPlan {
        match parse_registration(&words(s)) {
            Ok(RegistrationCall::Run(p)) => *p,
            other => panic!("{s}: {other:?}"),
        }
    }

    fn refused(s: &str) -> String {
        match parse_registration(&words(s)) {
            Err(e) => e.0,
            Ok(other) => panic!("{s}: accepted as {other:?}"),
        }
    }

    const LIN: &str = "-t Rigid[0.1] -m MI[f.nii.gz,m.nii.gz,1,32,Regular,0.25] \
                       -c [10x5,1e-6,10] -f 2x1 -s 1x0vox \
                       -t Affine[0.1] -m MI[f.nii.gz,m.nii.gz,1,32,Regular,0.25] \
                       -c [10x5,1e-6,10] -f 2x1 -s 1x0vox";
    const SYN: &str = "-t SyN[0.1,3,0] -m CC[f.nii.gz,m.nii.gz,1,2] -c [5x3,1e-6,10] -f 2x1 \
                       -s 1x0vox";

    fn full(extra: &str) -> String {
        format!("-d 3 -r [f.nii.gz,m.nii.gz,1] {LIN} {SYN} -o [z1/o_,W.nii.gz,IW.nii.gz] {extra}")
    }

    #[test]
    fn the_default_chain_maps_to_a_stage_list() {
        let p = plan(&full(""));
        assert_eq!(p.fixed, vec![PathBuf::from("f.nii.gz")]);
        assert_eq!(p.moving, vec![PathBuf::from("m.nii.gz")]);
        assert_eq!(p.initial, InitialArg::CentreOfMass);
        assert_eq!(p.stages.len(), 3);
        assert_eq!(p.winsorize, None);
        assert!(!p.composite && !p.float && !p.verbose);
        assert_eq!(p.interpolation, Interp::Linear);
        assert_eq!(
            p.output_files(),
            [
                "z1/o_0GenericAffine.mat",
                "z1/o_1Warp.nii.gz",
                "z1/o_1InverseWarp.nii.gz",
                "W.nii.gz",
                "IW.nii.gz"
            ]
            .map(PathBuf::from)
            .to_vec()
        );
        let want = json!({
            "initial": "com",
            "stages": [
                {"transform": "rigid", "gradient_step": 0.1, "iterations": [10, 5],
                 "convergence_threshold": 1e-6, "convergence_window": 10,
                 "shrink_factors": [2, 1], "smoothing_sigmas": [1.0, 0.0], "sigma_units": "vox",
                 "bins": 32, "sampling_percentage": 0.25},
                {"transform": "affine", "gradient_step": 0.1, "iterations": [10, 5],
                 "convergence_threshold": 1e-6, "convergence_window": 10,
                 "shrink_factors": [2, 1], "smoothing_sigmas": [1.0, 0.0], "sigma_units": "vox",
                 "bins": 32, "sampling_percentage": 0.25},
                {"transform": "syn", "gradient_step": 0.1, "update_field_variance": 3.0,
                 "total_field_variance": 0.0, "cc_radius": 2, "channel_weights": [1.0],
                 "iterations": [5, 3], "convergence_threshold": 1e-6, "convergence_window": 10,
                 "shrink_factors": [2, 1], "smoothing_sigmas": [1.0, 0.0], "sigma_units": "vox"}
            ],
            "winsorize": null
        });
        assert_eq!(p.recipe_json(), want);
        let r = Recipe::from_json(&p.recipe_json(), 1).unwrap();
        assert_eq!(r.plan.linear.len(), 2);
        assert_eq!(r.plan.winsorize, None);
        assert_eq!(r.plan.syn.as_ref().unwrap().cc_radius, 2);
    }

    #[test]
    fn metric_first_and_spaced_brackets_give_the_same_plan() {
        let a = plan(&full(""));
        let b = plan(
            "--dimensionality 3 --initial-moving-transform [ f.nii.gz , m.nii.gz , 1 ] \
             --metric MI[ f.nii.gz, m.nii.gz, 1, 32, Regular, 0.25 ] --transform Rigid[ 0.1 ] \
             --convergence [ 10x5,1e-6,10 ] --smoothing-sigmas 1x0vox --shrink-factors 2x1 \
             --metric mattes[f.nii.gz,m.nii.gz,1,32,regular,0.25] --transform affine[0.1] \
             --convergence [10x5,1e-6,10] --smoothing-sigmas 1x0 --shrink-factors 2x1 \
             --metric CC[f.nii.gz,m.nii.gz,1,2] --transform SyN[0.1] --convergence [5x3] \
             --smoothing-sigmas 1x0vox --shrink-factors 2x1 \
             --output [ z1/o_ , W.nii.gz , IW.nii.gz ]",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn the_benchmark_argv_parses() {
        let p = plan(
            "--dimensionality 3 --output [out/sub_] --winsorize-image-intensities [0.005,0.995] \
             --use-histogram-matching 0 --initial-moving-transform [f.nii.gz,m.nii.gz,1] \
             --transform Rigid[0.1] --metric MI[f.nii.gz,m.nii.gz,1,32,Regular,0.25] \
             --convergence [1000x500x250x100,1e-06,10] --shrink-factors 8x4x2x1 \
             --smoothing-sigmas 3x2x1x0vox \
             --transform Affine[0.1] --metric MI[f.nii.gz,m.nii.gz,1,32,Regular,0.25] \
             --convergence [1000x500x250x100,1e-06,10] --shrink-factors 8x4x2x1 \
             --smoothing-sigmas 3x2x1x0vox \
             --transform SyN[0.1,3,0] --metric CC[f.nii.gz,m.nii.gz,1,4] \
             --convergence [100x70x50x20,1e-06,10] --shrink-factors 8x4x2x1 \
             --smoothing-sigmas 3x2x1x0vox --write-composite-transform 1 \
             --random-seed 20260910 --verbose 1",
        );
        assert!(p.composite && p.verbose);
        assert_eq!(p.seed, Some(20260910));
        assert_eq!(p.winsorize, Some((0.005, 0.995)));
        assert_eq!(
            p.transform_files(),
            vec![
                PathBuf::from("out/sub_Composite.h5"),
                PathBuf::from("out/sub_InverseComposite.h5")
            ]
        );
        let r = Recipe::from_json(&p.recipe_json(), 1).unwrap();
        let cohort = Recipe::cohort(1);
        assert_eq!(r.plan, cohort.plan);
    }

    #[test]
    fn file_numbering_follows_ants() {
        let syn_only = plan(&format!("{SYN} -o o_"));
        assert_eq!(syn_only.initial, InitialArg::None);
        assert_eq!(
            syn_only.transform_files(),
            vec![
                PathBuf::from("o_0Warp.nii.gz"),
                PathBuf::from("o_0InverseWarp.nii.gz")
            ]
        );
        assert_eq!(syn_only.recipe_json()["initial"], json!("identity"));
        let identity = plan(&format!("-r Identity {SYN} -o o_"));
        assert_eq!(identity.initial, InitialArg::Identity);
        assert_eq!(identity.transform_files().len(), 3);
        assert_eq!(
            identity.transform_files()[1],
            PathBuf::from("o_1Warp.nii.gz")
        );
        let linear = plan(&format!("{LIN} -o noprefixsep/o"));
        assert_eq!(
            linear.output_files(),
            vec![PathBuf::from("noprefixsep/o0GenericAffine.mat")]
        );
        let start = plan(&format!("-r [start.mat,1] {SYN} -o o_ -a 1"));
        assert_eq!(
            start.initial,
            InitialArg::File {
                path: PathBuf::from("start.mat"),
                invert: true
            }
        );
        assert_eq!(
            start.recipe_json()["initial"],
            json!({"path": "start.mat", "invert": true})
        );
        assert_eq!(start.transform_files().len(), 2);
        assert_eq!(plan(&format!("-r start.txt {SYN} -o o_")).warp_index(), 1);
    }

    #[test]
    fn stage_options_are_assigned_as_ants_assigns_them() {
        let late_metric = plan(
            "-t Rigid[0.1] -c [2,1e-6,10] -m MI[f.nii,m.nii,1,32,Regular,0.25] -f 1 -s 0 -o o_",
        );
        assert_eq!(late_metric.stages.len(), 1);
        let StageSpec::Linear(l) = &late_metric.stages[0] else {
            panic!("not linear")
        };
        assert_eq!(l.iterations, vec![2]);
        let grouped = plan(
            "-t Rigid[0.1] -m MI[f.nii,m.nii,1,32,Regular,0.25] -t Affine[0.1] \
             -m MI[f.nii,m.nii,1,32,Regular,0.25] -c 2 -c 3 -f 1 -f 1 -s 0 -s 0 -o o_",
        );
        let its: Vec<Vec<u32>> = grouped
            .linear_stages()
            .map(|l| l.iterations.clone())
            .collect();
        assert_eq!(its, vec![vec![2], vec![3]]);
        let two = plan(
            "-m CC[f.nii,m.nii,0.5,2] -m CC[f2.nii,m2.nii,0.5,2] -t SyN[0.1] -c 2 -f 1 -s 0 -o o_",
        );
        assert_eq!(two.channels(), 2);
    }

    #[test]
    fn two_cc_metrics_are_two_channels() {
        let p = plan(
            "-t SyN[0.2,3,0] -m CC[t1f.nii,t1m.nii,0.5,4] -m CC[t2f.nii,t2m.nii,0.5,4] \
             -c 5 -f 1 -s 0 -o o_",
        );
        assert_eq!(p.channels(), 2);
        assert_eq!(p.moving[1], PathBuf::from("t2m.nii"));
        assert_eq!(p.syn_stage().unwrap().channel_weights, vec![0.5, 0.5]);
        assert_eq!(p.syn_stage().unwrap().gradient_step, 0.2);
        assert!(Recipe::from_json(&p.recipe_json(), 2).is_ok());
    }

    #[test]
    fn masks_options_seed_and_flags() {
        let p = plan(&format!(
            "{LIN} -x [mask.nii.gz,NULL] -o o_ --float -v -a 0 -z 1 -i 0 -u 0 -p 0 \
             --write-interval-volumes 0 --minc 0 -g 1x1x1 -n NearestNeighbor --random-seed 7"
        ));
        assert!(p.float && p.verbose && !p.composite);
        assert_eq!(p.interpolation, Interp::NearestNeighbor);
        assert_eq!(p.seed, Some(7));
        let masks: Vec<_> = p.linear_stages().map(|l| l.fixed_mask.clone()).collect();
        assert_eq!(masks, vec![Some(PathBuf::from("mask.nii.gz")); 2]);
        assert_eq!(p.recipe_json()["stages"][1]["seed"], json!(7));
        let per_stage = plan(&format!(
            "-r [f.nii.gz,m.nii.gz,1] {LIN} {SYN} -x NULL -x a.nii -x [NA,NONE] -o o_"
        ));
        let masks: Vec<_> = per_stage
            .linear_stages()
            .map(|l| l.fixed_mask.clone())
            .collect();
        assert_eq!(masks, vec![None, Some(PathBuf::from("a.nii"))]);
        let j = per_stage.to_json();
        assert_eq!(j["fixed_masks"], json!([null, "a.nii"]));
        assert_eq!(j["initial"], json!("com"));
        assert_eq!(j["files"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn help_and_version() {
        assert!(matches!(
            parse_registration(&words("-h")),
            Ok(RegistrationCall::Help(t)) if t.contains("ants-registration")
        ));
        assert!(matches!(
            parse_registration(&words("--version")),
            Ok(RegistrationCall::Version(t)) if t.contains("antsRegistration compatibility")
        ));
    }

    #[test]
    fn every_refusal_has_its_message() {
        let cases: Vec<(String, &str)> = vec![
            (full("").replace("-d 3", "-d 2"), "-d 2 is not supported: FastWarp registers and resamples 3-D images."),
            (full("").replace("-d 3", "-d 4"), "-d 4 is not supported: FastWarp registers and resamples 3-D images."),
            (LIN.to_string(), "--output is required"),
            (full("-j state.h5"), "-j is not supported: FastWarp cannot save or restore the state of a registration."),
            (full("--restore-state s.h5"), "--restore-state is not supported: FastWarp cannot save or restore the state of a registration."),
            (full("-p 5"), "-p 5 is not supported: FastWarp does not compute the full-resolution metric during the registration."),
            (full("--write-interval-volumes 2"), "--write-interval-volumes 2 is not supported: FastWarp does not write intermediate volumes."),
            (full("-z 0"), "-z 0 is not supported: FastWarp writes collapsed transforms only (one affine and one warp per direction)."),
            (full("-i 1"), "-i 1 is not supported: each linear stage starts from the identity centred on the previous result, as with 0."),
            (full("-n BSpline[3]"), "interpolation BSpline is not supported: use Linear, NearestNeighbor or LanczosWindowedSinc."),
            (full("-n GenericLabel"), "interpolation GenericLabel is not supported: use Linear, NearestNeighbor or LanczosWindowedSinc."),
            (full("-n Foo"), "unrecognized interpolation option foo"),
            (full("-g 1x1x0"), "-g 1x1x0 is not supported: FastWarp cannot restrict transform components."),
            (full("-q [f.nii.gz,m.nii.gz,1]"), "-q is not supported: FastWarp has no initial fixed transform."),
            (format!("-r [f.nii.gz,m.nii.gz,0] {SYN} -o o_"), "-r [f.nii.gz,m.nii.gz,0] is not supported: FastWarp initialises by the centres of intensity mass (feature 1) only."),
            (format!("-r [f.nii.gz,m.nii.gz,2] {SYN} -o o_"), "-r [f.nii.gz,m.nii.gz,2] is not supported: FastWarp initialises by the centres of intensity mass (feature 1) only."),
            (format!("-r [f.nii.gz,m.nii.gz,1] -r a.mat {SYN} -o o_"), "-r given 2 times is not supported: FastWarp starts from one initial transform."),
            (format!("-r [x.nii.gz,m.nii.gz,1] {SYN} -o o_"), "-r [x.nii.gz,m.nii.gz,1] with images other than the first metric's is not supported: FastWarp computes the centres of mass on the images of the first metric."),
            (format!("-r warp.nii.gz {SYN} -o o_"), "-r with a displacement field is not supported: FastWarp starts from one affine transform (.mat, .txt)."),
            (full("-u 1"), "-u 1 is not supported: FastWarp has no histogram matching."),
            (full("-u"), "-u needs a value, 0 or 1"),
            (full("--minc 1"), "--minc 1 is not supported: FastWarp writes ITK transform files."),
            (full("--foo 1"), "unknown option --foo"),
            (full("-x [f.nii.gz,m.nii.gz]"), "a moving-image mask in --masks is not supported: FastWarp has no moving-image mask."),
            (full("-x mask.nii.gz"), "a fixed-image mask on the SyN stage (--masks) is not supported: FastWarp's SyN stage has no mask; give NULL for that stage."),
            (full("-x NULL -x NULL -x a.nii -x NULL"), "--masks is given 4 times for 3 stages: give it once or once per stage"),
            (full("-x a.mha"), "image a.mha is not supported: FastWarp reads and writes NIfTI images (.nii, .nii.gz) only."),
            (full("-w [0.9,0.1]"), "-w [0.9,0.1]: expected quantiles with 0 <= lower < upper <= 1"),
            (full("-a 0 -a 1"), "-a is given 2 times; give it once"),
            ("-t Translation[0.1] -m MI[f.nii,m.nii,1,32,Regular,0.2] -c 5 -f 1 -s 0 -o o_".into(), "transform Translation is not supported: FastWarp has no Translation or Similarity transform; it runs Rigid, Affine and SyN."),
            ("-t Similarity[0.1] -m MI[f.nii,m.nii,1,32,Regular,0.2] -c 5 -f 1 -s 0 -o o_".into(), "transform Similarity is not supported: FastWarp has no Translation or Similarity transform; it runs Rigid, Affine and SyN."),
            ("-t BSplineSyN[0.1,26,0,3] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "transform BSplineSyN is not supported: FastWarp runs Rigid, Affine and SyN."),
            ("-t GaussianDisplacementField[0.1,3,0] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "transform GaussianDisplacementField is not supported: FastWarp runs Rigid, Affine and SyN."),
            ("-t TimeVaryingVelocityField[1,4,3,0,0,0] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "transform TimeVaryingVelocityField is not supported: FastWarp runs Rigid, Affine and SyN."),
            ("-t Exponential[0.1,3,0] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "transform Exponential is not supported: FastWarp runs Rigid, Affine and SyN."),
            ("-t Foo[0.1] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "unknown transform Foo[0.1]"),
            ("-t Rigid -m MI[f.nii,m.nii,1,32,Regular,0.2] -c 5 -f 1 -s 0 -o o_".into(), "-t Rigid: expected Rigid[gradientStep]"),
            ("-t SyN[0.1] -m MI[f.nii,m.nii,1,32] -c 5 -f 1 -s 0 -o o_".into(), "metric MI in a SyN stage is not supported: FastWarp's SyN stage uses CC."),
            ("-t SyN[0.1] -m Mattes[f.nii,m.nii,1,32,Regular,0.2] -c 5 -f 1 -s 0 -o o_".into(), "metric Mattes in a SyN stage is not supported: FastWarp's SyN stage uses CC."),
            ("-t SyN[0.1] -m MeanSquares[f.nii,m.nii,1,0] -c 5 -f 1 -s 0 -o o_".into(), "metric MeanSquares is not supported: FastWarp uses MI (Mattes) in Rigid and Affine stages and CC in the SyN stage."),
            ("-t SyN[0.1] -m Demons[f.nii,m.nii,1,0] -c 5 -f 1 -s 0 -o o_".into(), "metric Demons is not supported: FastWarp uses MI (Mattes) in Rigid and Affine stages and CC in the SyN stage."),
            ("-t Rigid[0.1] -m GC[f.nii,m.nii,1,1,Regular,0.25] -c 5 -f 1 -s 0 -o o_".into(), "metric GC is not supported: FastWarp uses MI (Mattes) in Rigid and Affine stages and CC in the SyN stage."),
            ("-t Rigid[0.1] -m ICP[f.vtk,m.vtk,1] -c 5 -f 1 -s 0 -o o_".into(), "metric ICP is not supported: FastWarp has no point-set metrics."),
            ("-t Rigid[0.1] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "metric CC in a Rigid stage is not supported: FastWarp's Rigid and Affine stages use MI (Mattes)."),
            ("-t Affine[0.1] -m MI[f.nii,m.nii,1,64,Regular,0.2] -c 5 -f 1 -s 0 -o o_".into(), "metric MI with 64 bins is not supported: the GPU kernels use 32 histogram bins."),
            ("-t Affine[0.1] -m MI[f.nii,m.nii,1,32] -c 5 -f 1 -s 0 -o o_".into(), "metric MI with dense sampling (no sampling strategy, or None) is not supported: FastWarp samples MI on a regular grid; add Regular and a percentage, for example MI[fixed,moving,1,32,Regular,0.25]."),
            ("-t Affine[0.1] -m MI[f.nii,m.nii,1,32,None] -c 5 -f 1 -s 0 -o o_".into(), "metric MI with dense sampling (no sampling strategy, or None) is not supported: FastWarp samples MI on a regular grid; add Regular and a percentage, for example MI[fixed,moving,1,32,Regular,0.25]."),
            ("-t Affine[0.1] -m MI[f.nii,m.nii,1,32,Random,0.2] -c 5 -f 1 -s 0 -o o_".into(), "metric MI with Random sampling is not supported: FastWarp samples MI on a regular grid; use Regular."),
            ("-t Affine[0.1] -m MI[f.nii,m.nii,1,32,Regular,1.5] -c 5 -f 1 -s 0 -o o_".into(), "-m MI[f.nii,m.nii,1,32,Regular,1.5]: the sampling percentage must be in (0, 1], got 1.5"),
            ("-t Affine[0.1] -m MI[f.nii,m.nii,1,32,Regular,0.2,1] -c 5 -f 1 -s 0 -o o_".into(), "metric MI with useGradientFilter 1 is not supported: FastWarp does not smooth the metric gradient."),
            ("-t Affine[0.1] -m MI[f.nii,m.nii,1,32,Regular,0.2] -m MI[f.nii,m.nii,1,32,Regular,0.2] -c 5 -f 1 -s 0 -o o_".into(), "stage 1: 2 metrics in the Affine stage is not supported: FastWarp's Rigid and Affine stages use one MI metric."),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,9] -c 5 -f 1 -s 0 -o o_".into(), "metric CC with radius 9 is not supported: the GPU kernels support radius 1 to 8."),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4,Regular,0.5] -c 5 -f 1 -s 0 -o o_".into(), "metric CC with Regular sampling is not supported: FastWarp's CC is dense; leave the sampling strategy out or give None."),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4] -m CC[a.nii,b.nii,1,2] -c 5 -f 1 -s 0 -o o_".into(), "stage 1: CC radii 4 and 2 is not supported: FastWarp's SyN stage uses one radius for both channels."),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4] -m CC[a.nii,b.nii,1,4] -m CC[c.nii,d.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "stage 1: 3 metrics in the SyN stage is not supported: FastWarp's SyN stage takes one or two CC metrics (channels)."),
            ("-t SyN[0.1] -m CC[f.mha,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "image f.mha is not supported: FastWarp reads and writes NIfTI images (.nii, .nii.gz) only."),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o [o_,w.mha]".into(), "image w.mha is not supported: FastWarp reads and writes NIfTI images (.nii, .nii.gz) only."),
            (format!("{SYN} {LIN} -o o_"), "stage order SyN, Rigid, Affine is not supported: FastWarp runs the linear stages first and at most one SyN stage, last."),
            (format!("{SYN} {SYN} -o o_"), "stage order SyN, SyN is not supported: FastWarp runs the linear stages first and at most one SyN stage, last."),
            (format!("-t Rigid[0.1] -m MI[f.nii.gz,x.nii.gz,1,32,Regular,0.25] -c 5 -f 1 -s 0 {SYN} -o o_"), "stage 1 with the images f.nii.gz, x.nii.gz is not supported: FastWarp's linear stages register the images of the first SyN metric (f.nii.gz, m.nii.gz)."),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4] -c [5,1e-6,1] -f 1 -s 0 -o o_".into(), "-c [5,1e-6,1]: the convergence window must be at least 2"),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4] -c [5x3,1e-6,10] -f 1 -s 0 -o o_".into(), "stage 1: --convergence gives 2 levels, --shrink-factors 1 and --smoothing-sigmas 1: give one value per level"),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4] -c 5 -f 0 -s 0 -o o_".into(), "-f 0: shrink factors must be at least 1"),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -o o_".into(), "--smoothing-sigmas is given 0 times for 1 --transform: give it once per stage"),
            ("-t SyN[0.1] -c 5 -f 1 -s 0 -o o_".into(), "the number of stages does not match up with the metrics: 1 --transform and 0 metric stages (a --metric starts a new stage unless it follows another --metric)"),
            ("-t SyN[0.1] -t SyN[0.1] -m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "the number of stages does not match up with the metrics: 2 --transform and 1 metric stages (a --metric starts a new stage unless it follows another --metric)"),
            ("-m CC[f.nii,m.nii,1,4] -c 5 -f 1 -s 0 -o o_".into(), "the number of stages does not match up with the metrics: 0 --transform and 1 metric stages (a --metric starts a new stage unless it follows another --metric)"),
            ("-m CC[f2.nii,m2.nii,1,2] -t SyN[0.1,3,0] -m CC[f.nii,m.nii,1,2] -c [3x2,1e-6,10] -f 2x1 -s 1x0vox -o o_".into(), "the number of stages does not match up with the metrics: 1 --transform and 2 metric stages (a --metric starts a new stage unless it follows another --metric)"),
            ("-t Rigid[0.1] -m MI[f.nii,m.nii,1,32,Regular,0.25] -c 2 -f 1 -s 0 -m MeanSquares[f.nii,m.nii,1,0] -t Affine[0.1] -m CC[f.nii,m.nii,1,2] -c 2 -f 1 -s 0 -o o_".into(), "the number of stages does not match up with the metrics: 2 --transform and 3 metric stages (a --metric starts a new stage unless it follows another --metric)"),
            ("-t SyN[0.1,3,0] -m CC[f2.nii,m2.nii,1,2] -x [NULL,NULL] -m CC[f.nii,m.nii,1,2] -c [3x2,1e-6,10] -f 2x1 -s 1x0vox -o o_".into(), "the number of stages does not match up with the metrics: 1 --transform and 2 metric stages (a --metric starts a new stage unless it follows another --metric)"),
            ("-t SyN[0.1,3,0] -m CC[f2.nii,m2.nii,1,2] -v 1 -m CC[f.nii,m.nii,1,2] -c [3x2,1e-6,10] -f 2x1 -s 1x0vox -o o_".into(), "the number of stages does not match up with the metrics: 1 --transform and 2 metric stages (a --metric starts a new stage unless it follows another --metric)"),
            ("-t Rigid[0.1] -m MI[f.nii,m.nii,1,32,Regular,0.25] -c 2 -c 3 -f 1 -s 0 -o o_".into(), "--convergence is given 2 times for 1 --transform: give it once per stage"),
            ("-d 3 -o o_".into(), "no stage given: each stage needs --transform, --metric, --convergence, --shrink-factors and --smoothing-sigmas"),
            ("-t SyN[0.1] -m CC[f.nii,m.nii,0,4] -c 5 -f 1 -s 0 -o o_".into(), "the weights of the SyN metrics must not all be 0"),
            (full("stray"), "unexpected argument \"stray\""),
        ];
        for (args, want) in cases {
            assert_eq!(refused(&args), want, "{args}");
        }
    }
}
