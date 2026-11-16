// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Runs a parsed `antsRegistration` / `antsApplyTransforms` call with the engine calls the
//! native `register`, `apply` and `compose` use, and writes the files ANTs writes.
//!
//! [`run_registration`] and [`run_apply_plan`] serve both the command line and the Python layer
//! `fastwarp.ants`: the images come from files, or from memory under the names in the arguments
//! ([`Images`]), so a Python call with `ANTsImage`s runs exactly what the subcommand runs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use fastwarp_reg::mask::MaskVolume;
use fastwarp_reg::register::write_linear_output;
use fastwarp_reg::stages::{InitialTransform, StagesOutput};
use fastwarp_reg::transform::itk_h5::write_h5_file;
use fastwarp_reg::transform::itk_mat::write_mat_file;
use fastwarp_reg::transform::{
    read_transform_file, AffineTransform, DisplacementField, TransformChain, TransformElem,
};
use fastwarp_reg::{Grid, Volume};
use serde_json::json;

use super::{
    interpolation_name, parse_apply, parse_registration, resolve_seed, ApplyCall, ApplyOutput,
    ApplyPlan, InitialArg, PixelType, Refusal, RegistrationCall, RegistrationPlan, StageSpec,
};
use crate::commands::print;
use crate::engine::{self, Failure, SynRun};
use crate::recipe::{kind_name, InitialSpec, Recipe};
use crate::{image, post, CliError, ExitStatus};

const REGISTRATION: &str = "ants-registration";
const APPLY: &str = "ants-apply-transforms";

fn refused(program: &str, r: Refusal) -> CliError {
    let message = format!("{program}: {}", r.0);
    if r.is_unsupported() {
        CliError::unsupported(message)
    } else {
        CliError::usage(message)
    }
}

fn failed(program: &str, e: impl Into<CliError>) -> CliError {
    let mut e = e.into();
    e.message = format!("{program}: {}", e.message);
    e
}

fn warn(err: &mut dyn Write, program: &str, warnings: &[String]) {
    for w in warnings {
        let _ = writeln!(err, "fastwarp: warning: {program}: {w}");
    }
    let _ = err.flush();
}

/// An input image of an ANTs call held in memory under the name the arguments give it.
#[derive(Clone, Debug)]
pub enum MemoryImage {
    /// An image: its voxels and grid.
    Volume(Volume<f64>),
    /// Only a grid, for a reference image.
    Grid(Grid),
}

/// An output image of an ANTs call kept in memory instead of written: the name the arguments
/// give it, its grid and its voxels (x fastest).
#[derive(Clone, Debug)]
pub struct KeptImage {
    pub name: PathBuf,
    pub grid: Grid,
    pub data: Vec<f32>,
}

/// Where the images of an ANTs call come from and go: files, except for the names given here.
/// An input name found in memory is never looked up on disk, and an output name to keep is
/// never written.
#[derive(Debug, Default)]
pub struct Images {
    inputs: Vec<(PathBuf, MemoryImage)>,
    keep: Vec<PathBuf>,
    kept: Vec<KeptImage>,
}

impl Images {
    /// Images from `inputs` by name (each image is taken once), outputs named in `keep` kept.
    pub fn new(inputs: Vec<(PathBuf, MemoryImage)>, keep: Vec<PathBuf>) -> Self {
        Self {
            inputs,
            keep,
            kept: Vec::new(),
        }
    }

    fn keeps(&self, path: &Path) -> bool {
        self.keep.iter().any(|p| p == path)
    }

    fn load(&mut self, path: &Path, role: &str) -> Result<Volume<f64>, CliError> {
        let Some(i) = self.inputs.iter().position(|(p, _)| p == path) else {
            return image::load(path, role);
        };
        match self.inputs.remove(i).1 {
            MemoryImage::Volume(v) => Ok(v),
            MemoryImage::Grid(_) => Err(CliError::usage(format!(
                "{role} {}: only its grid was given",
                path.display()
            ))),
        }
    }

    fn grid(&self, path: &Path, role: &str) -> Result<Grid, CliError> {
        match self.inputs.iter().find(|(p, _)| p == path) {
            Some((_, MemoryImage::Volume(v))) => Ok(*v.grid()),
            Some((_, MemoryImage::Grid(g))) => Ok(*g),
            None => image::grid(path, role),
        }
    }

    fn save(
        &mut self,
        path: &Path,
        grid: Grid,
        data: Vec<f32>,
        pixel: PixelType,
    ) -> Result<bool, CliError> {
        if self.keeps(path) {
            self.kept.push(KeptImage {
                name: path.to_path_buf(),
                grid,
                data,
            });
            return Ok(false);
        }
        image::save_as(path, grid, &data, pixel)?;
        Ok(true)
    }

    fn check_folder(&self, path: &Path) -> Result<(), CliError> {
        if self.keeps(path) {
            return Ok(());
        }
        check_folder(path)
    }

    /// The output images kept, in the order they were made.
    pub fn into_kept(self) -> Vec<KeptImage> {
        self.kept
    }
}

fn check_folder(path: &Path) -> Result<(), CliError> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() && !dir.is_dir() => Err(CliError::usage(format!(
            "output folder {} does not exist",
            dir.display()
        ))),
        _ => Ok(()),
    }
}

fn read_initial(path: &Path, invert: bool) -> Result<AffineTransform, CliError> {
    let what = format!("--initial-moving-transform {}", path.display());
    let mut chain = read_transform_file(path)
        .map_err(|e| CliError::new(ExitStatus::Io, format!("cannot read {what}: {e}")))?;
    if invert {
        chain = chain
            .inverse()
            .map_err(|e| CliError::usage(format!("{what}: cannot invert it ({e})")))?;
    }
    match chain.queue() {
        [TransformElem::Affine(a)] => Ok(*a),
        [TransformElem::DisplacementField(_)] => Err(CliError::unsupported(format!(
            "{what} with a displacement field is not supported: FastWarp starts from one affine \
             transform"
        ))),
        q => Err(CliError::unsupported(format!(
            "{what} with {} transforms is not supported: FastWarp starts from one affine \
             transform",
            q.len()
        ))),
    }
}

/// The stage plan of `plan` as a recipe, with the start file read and the masks loaded.
fn build_recipe(plan: &RegistrationPlan, images: &mut Images) -> Result<Recipe, CliError> {
    let channels = plan.channels();
    let mut v = plan.recipe_json();
    if matches!(plan.initial, InitialArg::File { .. }) {
        v["initial"] = json!("identity");
    }
    let mut recipe = Recipe::from_json(&v, channels).map_err(CliError::usage)?;
    if let InitialArg::File { path, invert } = &plan.initial {
        recipe.plan.initial = InitialTransform::Affine(read_initial(path, *invert)?);
        recipe.initial = InitialSpec::File {
            path: path.clone(),
            invert: *invert,
        };
        recipe.check(channels).map_err(CliError::usage)?;
    }
    let mut loaded: Vec<(PathBuf, MaskVolume)> = Vec::new();
    for (stage, spec) in recipe.plan.linear.iter_mut().zip(plan.linear_stages()) {
        let Some(path) = &spec.fixed_mask else {
            continue;
        };
        let mask = match loaded.iter().find(|(p, _)| p == path) {
            Some((_, m)) => m.clone(),
            None => {
                let m = MaskVolume::from_volume(images.load(path, "--masks")?);
                loaded.push((path.clone(), m.clone()));
                m
            }
        };
        stage.mask = Some(mask);
    }
    Ok(recipe)
}

fn field_of(chain: &TransformChain) -> Result<&DisplacementField, Failure> {
    chain
        .queue()
        .iter()
        .find_map(|t| match t {
            TransformElem::DisplacementField(f) => Some(f),
            TransformElem::Affine(_) => None,
        })
        .ok_or_else(|| {
            Failure::Registration("the registration returned no displacement field".into())
        })
}

fn write_failure(path: &Path, e: impl std::fmt::Display) -> Failure {
    Failure::Write(format!("cannot write {}: {e}", path.display()))
}

fn cli_failure(e: CliError) -> Failure {
    match e.status {
        ExitStatus::Io => Failure::Write(e.message),
        _ => Failure::Registration(e.message),
    }
}

/// `chain` with each displacement field resampled onto `grid` (linear interpolation, zero
/// outside the field's grid).
fn fields_on(chain: &TransformChain, grid: &fastwarp_reg::Grid) -> Result<TransformChain, Failure> {
    chain
        .queue()
        .iter()
        .map(|t| match t {
            TransformElem::Affine(_) => Ok(t.clone()),
            TransformElem::DisplacementField(f) => {
                if f.grid().approx_eq(grid, 1e-9) {
                    return Ok(t.clone());
                }
                post::compose_field(&TransformChain::from_queue(vec![t.clone()]), grid)
                    .map(TransformElem::DisplacementField)
                    .map_err(engine::reg_err)
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(TransformChain::from_queue)
}

fn write_registration(
    plan: &RegistrationPlan,
    out: &StagesOutput,
    fixed: Vec<Volume<f64>>,
    moving: Vec<Volume<f64>>,
    images: &mut Images,
) -> Result<Vec<PathBuf>, Failure> {
    let files = plan.transform_files();
    let native = *fixed[0].grid();
    let forward = fields_on(&out.pair.forward, &native)?;
    let inverse = fields_on(&out.pair.inverse, &native)?;
    if plan.composite {
        for (path, chain) in files.iter().zip([&forward, &inverse]) {
            if plan.has_affine_file() {
                write_h5_file(path, chain).map_err(|e| write_failure(path, e))?;
            } else {
                let only = TransformChain::from_queue(vec![TransformElem::DisplacementField(
                    field_of(chain)?.clone(),
                )]);
                write_h5_file(path, &only).map_err(|e| write_failure(path, e))?;
            }
        }
    } else {
        let mut rest = files.iter();
        if plan.has_affine_file() {
            let path = rest
                .next()
                .ok_or_else(|| Failure::Write("no affine file".into()))?;
            write_linear_output(&plan.prefix, &out.total).map_err(|e| write_failure(path, e))?;
        }
        if out.deformable {
            for (path, chain) in rest.zip([&forward, &inverse]) {
                post::write_field_as(path, field_of(chain)?, None, !plan.float)
                    .map_err(|e| write_failure(path, e))?;
            }
        }
    }
    let pixel = PixelType::internal(plan.float);
    let mut written = files;
    for (target, image, grid, chain) in [
        (&plan.warped, &moving[0], fixed[0].grid(), &forward),
        (&plan.inverse_warped, &fixed[0], moving[0].grid(), &inverse),
    ] {
        if let Some(path) = target {
            let data = post::resample(image, grid, chain, plan.interpolation, 0.0)
                .map_err(engine::reg_err)?;
            if images.save(path, *grid, data, pixel).map_err(cli_failure)? {
                written.push(path.clone());
            }
        }
    }
    Ok(written)
}

fn registration_report(plan: &RegistrationPlan, run: &SynRun, iso_mm: f64) -> String {
    let mut text = format!(
        "fastwarp {} antsRegistration compatibility mode on {}\nregistration grid {}x{}x{} at \
         {iso_mm} mm, {} channel(s), interpolation {}\n",
        env!("CARGO_PKG_VERSION"),
        run.adapter,
        run.grid[0],
        run.grid[1],
        run.grid[2],
        plan.channels(),
        interpolation_name(plan.interpolation),
    );
    let mut linear = run.linear_stages.iter();
    for (i, s) in plan.stages.iter().enumerate() {
        match s {
            StageSpec::Linear(l) => {
                let (iterations, metric) = linear
                    .next()
                    .map(|r| (r.iterations.clone(), r.metric))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "stage {} {}: iterations {iterations:?}, metric {}\n",
                    i + 1,
                    kind_name(l.kind),
                    metric.map_or("none".to_string(), |m| format!("{m:.6}"))
                ));
            }
            StageSpec::Syn(_) => {
                for l in &run.levels {
                    text.push_str(&format!(
                        "stage {} syn level {}: grid {}x{}x{}, iterations {}, energy {:.6}{}\n",
                        i + 1,
                        l.level,
                        l.grid_size[0],
                        l.grid_size[1],
                        l.grid_size[2],
                        l.iterations,
                        l.energy,
                        if l.converged { ", converged" } else { "" }
                    ));
                }
            }
        }
    }
    text.push_str(&format!(
        "linear {:.2} s, SyN {:.2} s, total {:.2} s\n",
        run.timings.linear_s, run.timings.syn_s, run.total_s
    ));
    for f in &run.files {
        text.push_str(&format!("wrote {}\n", f.display()));
    }
    text
}

/// `fastwarp ants-registration ARGS...`.
pub(crate) fn registration(
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitStatus, CliError> {
    let plan = match parse_registration(args).map_err(|r| refused(REGISTRATION, r))? {
        RegistrationCall::Help(t) | RegistrationCall::Version(t) => {
            print(out, &t)?;
            return Ok(ExitStatus::Ok);
        }
        RegistrationCall::Run(p) => *p,
    };
    warn(err, REGISTRATION, &plan.warnings);
    let env_seed = std::env::var_os("ANTS_RANDOM_SEED").map(|v| v.to_string_lossy().into_owned());
    let verbose = plan.verbose;
    let done = run_registration(
        plan,
        env_seed.as_deref(),
        Images::default(),
        RunOptions::default(),
    )
    .map_err(|e| failed(REGISTRATION, e))?;
    warn(err, REGISTRATION, &done.run.warnings);
    if verbose {
        print(out, &done.report)?;
    }
    Ok(ExitStatus::Ok)
}

/// What a registration run beyond its ANTs arguments uses: the registration grid's voxel size
/// (default: the fixed image's smallest spacing), the GPU and a flag that cancels the run.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub iso_mm: Option<f64>,
    pub adapter: Option<String>,
    pub cancel: Option<Arc<AtomicBool>>,
}

/// What [`run_registration`] did: the engine's report, the registration grid's voxel size, the
/// verbose text `-v 1` prints, and the output images kept in memory.
#[derive(Clone, Debug)]
pub struct RegistrationOutcome {
    pub run: SynRun,
    pub iso_mm: f64,
    pub report: String,
    pub kept: Vec<KeptImage>,
}

/// Runs a parsed `antsRegistration` plan as `fastwarp ants-registration` does: the seed from
/// `--random-seed`, else from `ants_random_seed` (the value of `ANTS_RANDOM_SEED`, passed in),
/// else FastWarp's default; the output folders checked, the start file, masks and images read
/// (from `images` where it holds them), the fixed images of two channels checked to share a
/// grid, all before the GPU is opened; then the registration and the files ANTs writes.
pub fn run_registration(
    mut plan: RegistrationPlan,
    ants_random_seed: Option<&str>,
    mut images: Images,
    options: RunOptions,
) -> Result<RegistrationOutcome, CliError> {
    plan.seed = Some(
        resolve_seed(plan.seed, ants_random_seed).map_err(|r| CliError::usage(r.to_string()))?,
    );
    for f in &plan.output_files() {
        images.check_folder(f)?;
    }
    let recipe = build_recipe(&plan, &mut images)?;
    let mut load = |paths: &[PathBuf], role: &str| -> Result<Vec<Volume<f64>>, CliError> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let role = if i == 0 {
                    role.to_string()
                } else {
                    format!("{role} of channel {}", i + 1)
                };
                images.load(p, &role)
            })
            .collect()
    };
    let fixed = load(&plan.fixed, "fixed image")?;
    let moving = load(&plan.moving, "moving image")?;
    for (i, f) in fixed.iter().enumerate().skip(1) {
        if !f.grid().approx_eq(fixed[0].grid(), 1e-9) {
            return Err(CliError::unsupported(format!(
                "the fixed image {} of SyN metric {} off the grid of {} is not supported: \
                 FastWarp's SyN channels share one fixed grid; resample it onto that grid first",
                plan.fixed[i].display(),
                i + 1,
                plan.fixed[0].display()
            )));
        }
    }
    let iso_mm = match options.iso_mm {
        Some(mm) => {
            engine::check_iso_mm(mm)?;
            mm
        }
        None => engine::finest_spacing(fixed[0].grid()),
    };
    let run = engine::register_with(
        fixed,
        moving,
        iso_mm,
        options.adapter,
        options.cancel,
        recipe,
        |o, f, m| {
            let t = std::time::Instant::now();
            let files = write_registration(&plan, o, f, m, &mut images)?;
            Ok((files, t.elapsed().as_secs_f64()))
        },
    )?;
    let report = registration_report(&plan, &run, iso_mm);
    Ok(RegistrationOutcome {
        run,
        iso_mm,
        report,
        kept: images.into_kept(),
    })
}

fn collapse_linear(chain: &TransformChain) -> Result<AffineTransform, CliError> {
    let mut total = AffineTransform::identity();
    for t in chain.queue() {
        match t {
            TransformElem::Affine(a) => total = AffineTransform::compose(&total, a),
            TransformElem::DisplacementField(_) => {
                return Err(CliError::unsupported(
                    "a Linear[...] output is not supported with a displacement field: it needs \
                     linear transforms only; use DisplacementField[...]",
                ))
            }
        }
    }
    Ok(total)
}

/// Runs a parsed `antsApplyTransforms` plan as `fastwarp ants-apply-transforms` does, with the
/// input and reference images taken from `images` where it holds them and an image output kept
/// there when it is named to keep. Returns the output images kept.
pub fn run_apply_plan(plan: &ApplyPlan, mut images: Images) -> Result<Vec<KeptImage>, CliError> {
    run_apply(plan, &mut images)?;
    Ok(images.into_kept())
}

fn run_apply(plan: &ApplyPlan, images: &mut Images) -> Result<(), CliError> {
    let target = plan.output.path();
    images.check_folder(target)?;
    let paths: Vec<PathBuf> = plan.transforms.iter().map(|t| t.path.clone()).collect();
    let flags: Vec<bool> = plan.transforms.iter().map(|t| t.invert).collect();
    let chain = if paths.is_empty() {
        TransformChain::new()
    } else {
        engine::guarded(|| engine::read_chain(&paths, Some(&flags)))?
    };
    let reference = plan.reference.clone();
    let reference_grid = |images: &Images| -> Result<Grid, CliError> {
        let path = reference
            .as_deref()
            .ok_or_else(|| CliError::usage("--reference-image is required for this output"))?;
        images.grid(path, "reference image")
    };
    let compose = |grid: &fastwarp_reg::Grid| -> Result<DisplacementField, CliError> {
        Ok(engine::guarded(|| {
            post::compose_field(&chain, grid).map_err(engine::reg_err)
        })?)
    };
    let write_err = |e: &dyn std::fmt::Display| {
        CliError::new(
            ExitStatus::Io,
            format!("cannot write {}: {e}", target.display()),
        )
    };
    match &plan.output {
        ApplyOutput::Image(path) => {
            let grid = reference_grid(images)?;
            let input = plan
                .input
                .as_deref()
                .ok_or_else(|| CliError::usage("--input is required to write a warped image"))?;
            let input = images.load(input, "input image")?;
            let data = engine::apply_chain(
                &input,
                &grid,
                &chain,
                plan.interpolation,
                plan.default_value,
            )?;
            images.save(path, grid, data, plan.output_type)?;
        }
        ApplyOutput::Field(path) => {
            let field = compose(&reference_grid(images)?)?;
            post::write_field_as(path, &field, None, plan.output_type == PixelType::Double)
                .map_err(|e| write_err(&e))?;
        }
        ApplyOutput::FieldH5(path) => {
            let field = compose(&reference_grid(images)?)?;
            let only = TransformChain::from_queue(vec![TransformElem::DisplacementField(field)]);
            write_h5_file(path, &only).map_err(|e| write_err(&e))?;
        }
        ApplyOutput::Composite(path) => {
            if chain.is_empty() {
                return Err(CliError::usage(
                    "a CompositeTransform[...] output needs at least one --transform",
                ));
            }
            write_h5_file(path, &chain).map_err(|e| write_err(&e))?;
        }
        ApplyOutput::Linear { path, invert } => {
            let mut a = collapse_linear(&chain)?;
            if *invert {
                a = a.inverse().map_err(|e| {
                    CliError::usage(format!("cannot invert the collapsed affine: {e}"))
                })?;
            }
            write_mat_file(path, &[TransformElem::Affine(a)]).map_err(|e| write_err(&e))?;
        }
    }
    Ok(())
}

/// `fastwarp ants-apply-transforms ARGS...`.
pub(crate) fn apply(
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitStatus, CliError> {
    let plan = match parse_apply(args).map_err(|r| refused(APPLY, r))? {
        ApplyCall::Help(t) | ApplyCall::Version(t) => {
            print(out, &t)?;
            return Ok(ExitStatus::Ok);
        }
        ApplyCall::Run(p) => *p,
    };
    warn(err, APPLY, &plan.warnings);
    run_apply(&plan, &mut Images::default()).map_err(|e| failed(APPLY, e))?;
    if plan.verbose {
        print(
            out,
            &format!(
                "fastwarp {} antsApplyTransforms compatibility mode\nwrote {}\n",
                env!("CARGO_PKG_VERSION"),
                plan.output.path().display()
            ),
        )?;
    }
    Ok(ExitStatus::Ok)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::new(
            [4, 3, 2],
            [1.0, 2.0, 1.5],
            [-3.0, 1.0, 2.0],
            fastwarp_reg::linalg::IDENTITY3,
        )
        .unwrap()
    }

    #[test]
    fn memory_images_are_read_and_kept_without_files() {
        let dir = std::env::temp_dir().join(format!("fastwarp_ants_memory_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mat = dir.join("identity.mat");
        write_mat_file(&mat, &[TransformElem::Affine(AffineTransform::identity())]).unwrap();
        let values: Vec<f64> = (0..24).map(f64::from).collect();
        let input = Volume::from_vec(grid(), values.clone()).unwrap();
        let args: Vec<String> = [
            "-d",
            "3",
            "-i",
            "<moving>.nii",
            "-r",
            "<fixed>.nii",
            "-o",
            "missing-folder/<warped>.nii",
            "-n",
            "NearestNeighbor",
            "-t",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([mat.to_string_lossy().into_owned()])
        .collect();
        let plan = match parse_apply(&args).unwrap() {
            ApplyCall::Run(p) => *p,
            other => panic!("{other:?}"),
        };
        let images = Images::new(
            vec![
                (PathBuf::from("<moving>.nii"), MemoryImage::Volume(input)),
                (PathBuf::from("<fixed>.nii"), MemoryImage::Grid(grid())),
            ],
            vec![PathBuf::from("missing-folder/<warped>.nii")],
        );
        let kept = run_apply_plan(&plan, images).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, PathBuf::from("missing-folder/<warped>.nii"));
        assert!(kept[0].grid.approx_eq(&grid(), 0.0));
        let back: Vec<f64> = kept[0].data.iter().map(|v| f64::from(*v)).collect();
        assert_eq!(back, values);
        assert!(!Path::new("missing-folder").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_grid_is_not_an_image() {
        let mut images = Images::new(
            vec![(PathBuf::from("<r>.nii"), MemoryImage::Grid(grid()))],
            Vec::new(),
        );
        assert!(images.grid(Path::new("<r>.nii"), "reference").is_ok());
        let e = images
            .load(Path::new("<r>.nii"), "input image")
            .unwrap_err();
        assert_eq!(e.status, ExitStatus::Usage);
        assert!(e.message.contains("only its grid"), "{}", e.message);
    }
}
