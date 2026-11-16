// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The arguments of the `fastwarp` command line.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// GPU image registration (Rigid, Affine, SyN) with ANTs-compatible outputs.
///
/// Exit status: 0 success, 1 registration failed, 2 invalid arguments, 3 file could not be read
/// or written, 4 no usable GPU, 5 the grid does not fit the GPU, 6 cancelled.
#[derive(Parser, Debug)]
#[command(
    name = "fastwarp",
    bin_name = "fastwarp",
    version,
    max_term_width = 100
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Registration of MOVING to FIXED: Rigid, Affine and SyN by default, any start, linear stages
    /// and optional SyN stage with a stage-list recipe.
    Register(RegisterArgs),
    /// One rigid or affine fit of MOVING to FIXED (Mattes mutual information).
    Linear(LinearArgs),
    /// Resample an image onto a reference grid through transforms (antsApplyTransforms).
    Apply(ApplyArgs),
    /// Write transforms as one displacement field on a reference grid
    /// (antsApplyTransforms --compose).
    Compose(ComposeArgs),
    /// Print the default registration recipe as JSON.
    Recipe(RecipeArgs),
    /// Describe the GPU FastWarp would use, as JSON.
    GpuInfo(GpuInfoArgs),
    /// Check whether a registration fits the GPU (exit 0 if it fits, 5 if not).
    Fits(FitsArgs),
    /// Time FastWarp registrations on synthetic volumes or on your images.
    Bench(BenchArgs),
    /// Run antsRegistration arguments (ANTs 2.6.5, supported subset).
    ///
    /// Takes the arguments of antsRegistration, runs the subset FastWarp supports and writes the
    /// files antsRegistration writes; anything else is refused with exit status 2.
    /// `fastwarp ants-registration --help` lists the subset.
    #[command(name = "ants-registration", disable_help_flag = true)]
    AntsRegistration(AntsArgs),
    /// Run antsApplyTransforms arguments (ANTs 2.6.5, supported subset).
    ///
    /// Takes the arguments of antsApplyTransforms and writes what it writes; anything else is
    /// refused with exit status 2. `fastwarp ants-apply-transforms --help` lists the subset.
    #[command(name = "ants-apply-transforms", disable_help_flag = true)]
    AntsApplyTransforms(AntsArgs),
}

/// The arguments of a compatibility-mode command, passed through unparsed.
#[derive(Args, Debug)]
pub struct AntsArgs {
    /// The arguments of the ANTs program.
    #[arg(
        value_name = "ARGS",
        num_args = 0..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub args: Vec<String>,
}

#[derive(Args, Debug)]
pub struct RegisterArgs {
    /// Fixed image (NIfTI).
    pub fixed: PathBuf,
    /// Moving image (NIfTI).
    pub moving: PathBuf,
    /// Output prefix: writes <PREFIX>Composite.h5, <PREFIX>InverseComposite.h5 and
    /// <PREFIX>0GenericAffine.mat (see --outputs). A prefix ending in a path separator is a
    /// folder.
    #[arg(short = 'o', long = "output", value_name = "PREFIX")]
    pub output: PathBuf,
    /// Transform files: composite (Composite.h5, InverseComposite.h5, 0GenericAffine.mat), split
    /// (what antsRegistration writes without --write-composite-transform: 0GenericAffine.mat and,
    /// after a SyN stage, 1Warp.nii.gz and 1InverseWarp.nii.gz) or both.
    #[arg(long, value_enum, default_value_t = Outputs::Composite)]
    pub outputs: Outputs,
    /// Second fixed channel (for example a T2w image) for the SyN stage.
    #[arg(long, value_name = "F2", requires = "moving2")]
    pub fixed2: Option<PathBuf>,
    /// Second moving channel, already in the space of MOVING.
    #[arg(long, value_name = "M2", requires = "fixed2")]
    pub moving2: Option<PathBuf>,
    /// Spacing of the isotropic registration grid in millimetres. Default: the smallest voxel
    /// spacing of FIXED (the Python function register_syn defaults to 1.0 mm instead).
    #[arg(long, value_name = "MM")]
    pub iso_mm: Option<f64>,
    /// Recipe as a JSON file: the form `fastwarp recipe` prints, or the stage-list form
    /// {"initial": ..., "stages": [...]}; fields left out keep the default.
    #[arg(long, value_name = "FILE")]
    pub recipe: Option<PathBuf>,
    /// Also write <PREFIX>_Warped.nii.gz, MOVING on the registration grid.
    #[arg(long)]
    pub write_warped: bool,
    #[command(flatten)]
    pub common: Common,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outputs {
    Composite,
    Split,
    Both,
}

impl Outputs {
    pub fn name(self) -> &'static str {
        match self {
            Outputs::Composite => "composite",
            Outputs::Split => "split",
            Outputs::Both => "both",
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearTransform {
    Rigid,
    Affine,
}

impl LinearTransform {
    pub fn name(self) -> &'static str {
        match self {
            LinearTransform::Rigid => "rigid",
            LinearTransform::Affine => "affine",
        }
    }
}

#[derive(Args, Debug)]
pub struct LinearArgs {
    /// Fixed image (NIfTI).
    pub fixed: PathBuf,
    /// Moving image (NIfTI).
    pub moving: PathBuf,
    /// Output prefix: writes <PREFIX>0GenericAffine.mat. Without it nothing is written.
    #[arg(short = 'o', long = "output", value_name = "PREFIX")]
    pub output: Option<PathBuf>,
    /// Transform to fit.
    #[arg(long, value_enum, default_value_t = LinearTransform::Rigid)]
    pub transform: LinearTransform,
    /// Fixed-image mask (NIfTI, any grid): the metric samples only its non-zero voxels.
    #[arg(long, value_name = "MASK")]
    pub mask: Option<PathBuf>,
    /// Winsorize both images at these intensity quantiles, for example 0.005,0.995.
    #[arg(long, value_name = "LOWER,UPPER", value_parser = parse_winsorize)]
    pub winsorize: Option<(f32, f32)>,
    /// Start from this affine transform file (.mat or .txt, mapping fixed points to moving
    /// points) instead of the centres of intensity mass.
    #[arg(long, value_name = "FILE")]
    pub initial: Option<PathBuf>,
    /// Stage schedule as a JSON file (the fields of the "rigid" entry of `fastwarp recipe`).
    #[arg(long, value_name = "FILE")]
    pub stage: Option<PathBuf>,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug)]
pub struct ApplyArgs {
    /// Reference image: its grid is the output grid.
    #[arg(short = 'r', long, value_name = "REFERENCE")]
    pub reference: PathBuf,
    /// Image to resample.
    #[arg(short = 'i', long, value_name = "INPUT")]
    pub input: PathBuf,
    /// Output image (NIfTI, float32).
    #[arg(short = 'o', long, value_name = "OUTPUT")]
    pub output: PathBuf,
    /// Transform file (.h5, .mat, .txt, or a .nii/.nii.gz displacement field). Repeat it as in
    /// antsApplyTransforms: the first one given maps the reference point first. [FILE,1] uses
    /// the inverse of a linear transform (a displacement field cannot be inverted).
    #[arg(
        short = 't',
        long = "transform",
        value_name = "FILE",
        required = true,
        value_parser = parse_transform
    )]
    pub transforms: Vec<TransformArg>,
    /// Interpolator: linear, nearestNeighbor or lanczosWindowedSinc.
    #[arg(short = 'n', long, value_name = "NAME", default_value = "linear")]
    pub interpolation: String,
    /// Value of points that map outside the input image.
    #[arg(
        short = 'f',
        long,
        value_name = "VALUE",
        default_value_t = 0.0,
        allow_negative_numbers = true
    )]
    pub default_value: f64,
}

#[derive(Args, Debug)]
pub struct ComposeArgs {
    /// Reference image: the field is written on its grid.
    #[arg(short = 'r', long, value_name = "REFERENCE")]
    pub reference: PathBuf,
    /// Output displacement field (.nii or .nii.gz).
    #[arg(short = 'o', long, value_name = "FIELD")]
    pub output: PathBuf,
    /// Transform file, repeated as for `fastwarp apply` ([FILE,1] for the inverse of a linear
    /// transform).
    #[arg(
        short = 't',
        long = "transform",
        value_name = "FILE",
        required = true,
        value_parser = parse_transform
    )]
    pub transforms: Vec<TransformArg>,
    /// Gzip level 0 to 9 of a .nii.gz output (0 stores without compressing).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(0..=9))]
    pub compression_level: Option<u32>,
}

#[derive(Args, Debug)]
pub struct RecipeArgs {
    /// Number of channels (1, or 2 for a multispectral SyN stage).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=2))]
    pub channels: u8,
}

#[derive(Args, Debug)]
pub struct GpuInfoArgs {
    /// Pick the GPU whose name contains this text (overrides FASTWARP_GPU_ADAPTER).
    #[arg(long, value_name = "NAME")]
    pub adapter: Option<String>,
    /// Accepted for symmetry with the other commands; the output is always JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct FitsArgs {
    /// Fixed image (NIfTI; only the header is read).
    pub fixed: PathBuf,
    /// Moving image (NIfTI; only the header is read). Without it the moving grid is assumed to
    /// be the fixed grid.
    pub moving: Option<PathBuf>,
    /// Spacing of the isotropic registration grid in millimetres. Default: the smallest voxel
    /// spacing of FIXED, as for `fastwarp register` (fastwarp.fits_gpu defaults to 1.0 mm).
    #[arg(long, value_name = "MM")]
    pub iso_mm: Option<f64>,
    /// Number of channels (1, or 2 for a multispectral SyN stage).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=2))]
    pub channels: u8,
    /// Recipe as a JSON file, as for `fastwarp register`: the check covers its stages (a chain
    /// without a SyN stage needs far less memory).
    #[arg(long, value_name = "FILE")]
    pub recipe: Option<PathBuf>,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug)]
pub struct BenchArgs {
    /// Fixed image (NIfTI) to time instead of the synthetic volumes; give MOVING too.
    #[arg(requires = "moving")]
    pub fixed: Option<PathBuf>,
    /// Moving image (NIfTI).
    pub moving: Option<PathBuf>,
    /// Synthetic size to run; repeat for several. Default: mni-1mm and mni-0.5mm.
    #[arg(
        long = "size",
        value_name = "NAME",
        value_enum,
        conflicts_with = "fixed"
    )]
    pub sizes: Vec<crate::bench::Size>,
    /// Registration grid spacing in millimetres for FIXED / MOVING. Default: the smallest voxel
    /// spacing of FIXED, as for `fastwarp register`. Synthetic volumes register at their own
    /// spacing.
    #[arg(long, value_name = "MM", requires = "fixed")]
    pub iso_mm: Option<f64>,
    /// Timed runs per size.
    #[arg(long, value_name = "N", default_value_t = 1,
          value_parser = clap::value_parser!(u32).range(1..))]
    pub repeat: u32,
    /// Untimed warm-up runs before the timed ones. Default: 1 when --repeat is above 1, else 0.
    #[arg(long, value_name = "N")]
    pub warmup: Option<u32>,
    /// Cap on the CPU threads FastWarp uses for its host-side work (image resampling, intensity
    /// preprocessing, reading the input images); the GPU work is not limited. Default: all
    /// logical cores, or RAYON_NUM_THREADS.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub threads: Option<u32>,
    /// Recipe as a JSON file; fields left out keep the default.
    #[arg(long, value_name = "FILE")]
    pub recipe: Option<PathBuf>,
    #[command(flatten)]
    pub common: Common,
}

/// Options most commands share.
#[derive(Args, Debug, Default)]
pub struct Common {
    /// Pick the GPU whose name contains this text (overrides FASTWARP_GPU_ADAPTER).
    #[arg(long, value_name = "NAME")]
    pub adapter: Option<String>,
    /// Print the result as JSON on standard output.
    #[arg(long)]
    pub json: bool,
}

/// One `-t` value: a transform file, used inverted with `[FILE,1]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransformArg {
    pub path: PathBuf,
    pub invert: bool,
}

/// `FILE`, `[FILE]`, `[FILE,0]` or `[FILE,1]` (antsApplyTransforms' `useInverse` flag; `true` /
/// `false` are accepted as well).
pub fn parse_transform(s: &str) -> Result<TransformArg, String> {
    let t = s.trim();
    let Some(inner) = t.strip_prefix('[') else {
        if t.is_empty() {
            return Err("expected a transform file".into());
        }
        return Ok(TransformArg {
            path: PathBuf::from(t),
            invert: false,
        });
    };
    let inner = inner
        .strip_suffix(']')
        .ok_or_else(|| format!("{s:?}: expected [FILE,0] or [FILE,1]"))?;
    let (path, invert) = match inner.rsplit_once(',') {
        Some((p, f)) => match f.trim() {
            "0" | "false" => (p.trim(), false),
            "1" | "true" => (p.trim(), true),
            other if other.chars().all(|c| c.is_ascii_digit()) => {
                return Err(format!("{s:?}: the inverse flag is 0 or 1, not {other:?}"))
            }
            _ => (inner.trim(), false),
        },
        None => (inner.trim(), false),
    };
    if path.is_empty() {
        return Err(format!("{s:?}: expected a transform file"));
    }
    Ok(TransformArg {
        path: PathBuf::from(path),
        invert,
    })
}

/// `LOWER,UPPER` quantiles, `0 <= LOWER < UPPER <= 1`.
pub fn parse_winsorize(s: &str) -> Result<(f32, f32), String> {
    let (lo, hi) = s
        .split_once(',')
        .ok_or_else(|| "expected LOWER,UPPER, for example 0.005,0.995".to_string())?;
    let num = |t: &str| {
        t.trim()
            .parse::<f32>()
            .map_err(|_| format!("{t:?} is not a number"))
    };
    let (lo, hi) = (num(lo)?, num(hi)?);
    if !(0.0 <= lo && lo < hi && hi <= 1.0) {
        return Err("expected quantiles with 0 <= LOWER < UPPER <= 1".into());
    }
    Ok((lo, hi))
}
