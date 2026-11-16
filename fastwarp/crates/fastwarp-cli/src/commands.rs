// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The native subcommands.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fastwarp_reg::mask::MaskVolume;
use fastwarp_reg::register::write_linear_output;
use fastwarp_reg::transform::ants_io::{with_suffix, AFFINE_SUFFIX, COMPOSITE_SUFFIX};
use fastwarp_reg::transform::{read_transform_file, AffineTransform, TransformElem};
use serde_json::{json, Value};

use crate::args::{
    ApplyArgs, Command, ComposeArgs, FitsArgs, GpuInfoArgs, LinearArgs, RecipeArgs, RegisterArgs,
    TransformArg,
};
use crate::engine::{self, SynOutputs, SynRun};
use crate::image;
use crate::recipe::{linear_stage_from_json, linear_to_json, Recipe};
use crate::{CliError, ExitStatus};

pub(crate) fn dispatch(
    command: Command,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitStatus, CliError> {
    match command {
        Command::Register(a) => register(a, out, err),
        Command::Linear(a) => linear(a, out, err),
        Command::Apply(a) => apply(a, out),
        Command::Compose(a) => compose(a, out),
        Command::Recipe(a) => recipe(a, out),
        Command::GpuInfo(a) => gpu_info(a, out),
        Command::Fits(a) => fits(a, out),
        Command::Bench(a) => crate::bench::run(a, out, err),
        Command::AntsRegistration(a) => crate::ants::run::registration(&a.args, out, err),
        Command::AntsApplyTransforms(a) => crate::ants::run::apply(&a.args, out, err),
    }
}

/// Prints each engine warning on `err` as `fastwarp: warning: ...`.
pub(crate) fn warn(err: &mut dyn Write, warnings: &[String]) {
    for w in warnings {
        let _ = writeln!(err, "fastwarp: warning: {w}");
    }
    let _ = err.flush();
}

pub(crate) fn print(out: &mut dyn Write, text: &str) -> Result<(), CliError> {
    out.write_all(text.as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| CliError::new(ExitStatus::Io, format!("writing the output: {e}")))
}

pub(crate) fn print_json(out: &mut dyn Write, v: &Value) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(v)
        .map_err(|e| CliError::new(ExitStatus::Registration, e.to_string()))?;
    print(out, &format!("{text}\n"))
}

pub(crate) fn read_json(path: &Path, what: &str) -> Result<Value, CliError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        CliError::new(
            ExitStatus::Io,
            format!("cannot read the {what} file {}: {e}", path.display()),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| {
        CliError::usage(format!(
            "{what} file {}: not valid JSON: {e}",
            path.display()
        ))
    })
}

pub(crate) fn load_recipe(path: Option<&Path>, channels: usize) -> Result<Recipe, CliError> {
    match path {
        None => Ok(Recipe::cohort(channels)),
        Some(p) => {
            let v = read_json(p, "recipe")?;
            let base = p.parent().unwrap_or_else(|| Path::new(""));
            let v = crate::recipe::resolve_initial_path(&v, base);
            Recipe::from_json(&v, channels)
                .map_err(|e| CliError::usage(format!("recipe file {}: {e}", p.display())))
        }
    }
}

/// Creates the folder of an output prefix, as the Python functions do.
fn prepare_prefix(prefix: &Path) -> Result<(), CliError> {
    image::create_parent(&with_suffix(prefix, COMPOSITE_SUFFIX))
}

fn path_text(p: &Path) -> String {
    p.display().to_string()
}

fn grid_text(g: [usize; 3]) -> String {
    format!("{}x{}x{}", g[0], g[1], g[2])
}

pub(crate) fn syn_run_json(run: &SynRun) -> Value {
    let t = &run.timings;
    json!({
        "adapter": run.adapter,
        "grid": run.grid,
        "total_s": run.total_s,
        "resample_s": t.resample_s,
        "preprocess_s": t.preprocess_s,
        "linear_s": t.linear_s,
        "syn_s": t.syn_s,
        "warp_s": t.warp_s,
        "write_s": run.write_s,
        "levels": run.levels.iter().map(|l| json!({
            "level": l.level,
            "grid": l.grid_size,
            "iterations": l.iterations,
            "converged": l.converged,
            "energy": l.energy,
        })).collect::<Vec<_>>(),
        "affine_parameters": run.affine_parameters,
        "affine_center": run.affine_center,
        "initial_parameters": run.initial_parameters,
        "initial_center": run.initial_center,
        "linear_stages": run.linear_stages.iter().map(|s| json!({
            "transform": s.transform,
            "iterations": s.iterations,
            "metric": s.metric,
        })).collect::<Vec<_>>(),
        "deformable": run.deformable,
        "warnings": run.warnings,
        "files": run.files.iter().map(|p| path_text(p)).collect::<Vec<_>>(),
        "peak_gpu_bytes": run.peak_gpu_bytes,
        "memory_budget_bytes": run.memory_budget_bytes,
        "recipe": run.recipe.to_json(),
    })
}

fn register(
    a: RegisterArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitStatus, CliError> {
    if let Some(mm) = a.iso_mm {
        engine::check_iso_mm(mm)?;
    }
    let mut fixed_paths = vec![a.fixed.clone()];
    let mut moving_paths = vec![a.moving.clone()];
    fixed_paths.extend(a.fixed2.clone());
    moving_paths.extend(a.moving2.clone());
    engine::check_channels(fixed_paths.len(), moving_paths.len())?;
    let recipe = load_recipe(a.recipe.as_deref(), fixed_paths.len())?;
    for (i, p) in fixed_paths.iter().enumerate() {
        image::check_nifti_name(p, if i == 0 { "fixed image" } else { "--fixed2" })?;
    }
    for (i, p) in moving_paths.iter().enumerate() {
        image::check_nifti_name(p, if i == 0 { "moving image" } else { "--moving2" })?;
    }
    let fixed = fixed_paths
        .iter()
        .zip(["fixed image", "--fixed2"])
        .map(|(p, role)| image::load(p, role))
        .collect::<Result<Vec<_>, _>>()?;
    let moving = moving_paths
        .iter()
        .zip(["moving image", "--moving2"])
        .map(|(p, role)| image::load(p, role))
        .collect::<Result<Vec<_>, _>>()?;
    let outputs_layout = engine::output_layout(a.outputs.name())?;
    prepare_prefix(&a.output)?;
    engine::check_out_folder(&a.output)?;
    let iso_mm = a
        .iso_mm
        .unwrap_or_else(|| engine::finest_spacing(fixed[0].grid()));
    let outputs = SynOutputs {
        prefix: a.output.clone(),
        write_warped: a.write_warped,
        layout: outputs_layout,
    };
    let run = engine::register_syn(
        fixed,
        moving,
        Some(outputs),
        iso_mm,
        a.common.adapter.clone(),
        None,
        recipe,
    )?;
    warn(err, &run.warnings);
    let files = &run.files;
    if a.common.json {
        let mut v = syn_run_json(&run);
        v["outputs"] = json!(files.iter().map(|p| path_text(p)).collect::<Vec<_>>());
        print_json(out, &v)?;
    } else {
        let t = &run.timings;
        let mut text = format!(
            "registered {} to {} on {}\ngrid {} at {} mm; linear {:.1} s, SyN {:.1} s, total \
             {:.1} s\n",
            a.moving.display(),
            a.fixed.display(),
            run.adapter,
            grid_text(run.grid),
            iso_mm,
            t.linear_s,
            t.syn_s,
            run.total_s
        );
        for f in files {
            text.push_str(&format!("wrote {}\n", f.display()));
        }
        print(out, &text)?;
    }
    Ok(ExitStatus::Ok)
}

/// The paths of `-t` values and their inverse flags (`None` when none is inverted).
fn transform_paths(t: &[TransformArg]) -> (Vec<PathBuf>, Option<Vec<bool>>) {
    let paths = t.iter().map(|a| a.path.clone()).collect();
    let flags: Vec<bool> = t.iter().map(|a| a.invert).collect();
    (paths, flags.iter().any(|f| *f).then_some(flags))
}

fn read_initial(path: &Path) -> Result<AffineTransform, CliError> {
    let chain = read_transform_file(path).map_err(|e| {
        CliError::new(
            ExitStatus::Io,
            format!("cannot read --initial {}: {e}", path.display()),
        )
    })?;
    match chain.queue() {
        [TransformElem::Affine(a)] => Ok(*a),
        _ => Err(CliError::usage(format!(
            "--initial {}: expected a file with one affine transform",
            path.display()
        ))),
    }
}

fn linear(a: LinearArgs, out: &mut dyn Write, err: &mut dyn Write) -> Result<ExitStatus, CliError> {
    let transform = a.transform.name();
    let stage_json = a
        .stage
        .as_deref()
        .map(|p| read_json(p, "stage"))
        .transpose()?;
    let stage = linear_stage_from_json(stage_json.as_ref(), transform).map_err(|e| {
        CliError::usage(match &a.stage {
            Some(p) => format!("stage file {}: {e}", p.display()),
            None => e,
        })
    })?;
    engine::check_winsorize(a.winsorize)?;
    image::check_nifti_name(&a.fixed, "fixed image")?;
    image::check_nifti_name(&a.moving, "moving image")?;
    if let Some(m) = &a.mask {
        image::check_nifti_name(m, "--mask")?;
    }
    let initial = a.initial.as_deref().map(read_initial).transpose()?;
    let fixed = image::load(&a.fixed, "fixed image")?;
    let moving = image::load(&a.moving, "moving image")?;
    let mask = a
        .mask
        .as_deref()
        .map(|p| image::load(p, "--mask").map(MaskVolume::from_volume))
        .transpose()?;
    if let Some(prefix) = &a.output {
        image::create_parent(&with_suffix(prefix, AFFINE_SUFFIX))?;
    }
    let use_mask = mask.is_some();
    let mut prepared =
        engine::open_linear(fixed, moving, mask, a.winsorize, a.common.adapter.clone())?;
    let start = initial.unwrap_or(prepared.com);
    let t = Instant::now();
    let mask = if use_mask {
        prepared.session.mask.clone()
    } else {
        None
    };
    let fit = engine::guarded(|| {
        prepared
            .session
            .fit(&stage, start, mask, None)
            .map_err(engine::reg_err)
    })?;
    let total_s = t.elapsed().as_secs_f64();
    warn(err, &fit.warnings);
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(prefix) = &a.output {
        let path = with_suffix(prefix, AFFINE_SUFFIX);
        write_linear_output(prefix, &fit.total).map_err(|e| {
            CliError::new(
                ExitStatus::Io,
                format!("cannot write {}: {e}", path.display()),
            )
        })?;
        files.push(path);
    }
    if a.common.json {
        print_json(
            out,
            &json!({
                "adapter": prepared.adapter,
                "parameters": fit.total.parameters(),
                "center": fit.total.center,
                "initial_parameters": start.parameters(),
                "initial_center": start.center,
                "metric": fit.metric,
                "iterations": fit.iterations,
                "warnings": fit.warnings,
                "total_s": total_s,
                "transform": transform,
                "stage": linear_to_json(&stage),
                "outputs": files.iter().map(|p| path_text(p)).collect::<Vec<_>>(),
            }),
        )?;
    } else {
        let p = fit.total.parameters();
        let metric = fit
            .metric
            .map(|m| format!("{m:.6}"))
            .unwrap_or_else(|| "none".into());
        let mut text = format!(
            "{transform} fit of {} to {} on {}\nmetric {metric}, iterations {:?}, {:.1} s\n\
             matrix {:.6} {:.6} {:.6} / {:.6} {:.6} {:.6} / {:.6} {:.6} {:.6}\n\
             translation {:.4} {:.4} {:.4}, center {:.4} {:.4} {:.4}\n",
            a.moving.display(),
            a.fixed.display(),
            prepared.adapter,
            fit.iterations,
            total_s,
            p[0],
            p[1],
            p[2],
            p[3],
            p[4],
            p[5],
            p[6],
            p[7],
            p[8],
            p[9],
            p[10],
            p[11],
            fit.total.center[0],
            fit.total.center[1],
            fit.total.center[2],
        );
        for f in &files {
            text.push_str(&format!("wrote {}\n", f.display()));
        }
        print(out, &text)?;
    }
    Ok(ExitStatus::Ok)
}

fn apply(a: ApplyArgs, out: &mut dyn Write) -> Result<ExitStatus, CliError> {
    let interp = engine::interpolator(&a.interpolation)?;
    image::check_nifti_name(&a.reference, "reference image")?;
    image::check_nifti_name(&a.input, "input image")?;
    image::check_nifti_name(&a.output, "output image")?;
    let grid = image::grid(&a.reference, "reference image")?;
    let (paths, invert) = transform_paths(&a.transforms);
    let chain = engine::guarded(|| engine::read_chain(&paths, invert.as_deref()))?;
    let input = image::load(&a.input, "input image")?;
    let data = engine::apply_chain(&input, &grid, &chain, interp, a.default_value)?;
    image::create_parent(&a.output)?;
    image::save(&a.output, grid, data)?;
    print(out, &format!("wrote {}\n", a.output.display()))?;
    Ok(ExitStatus::Ok)
}

fn compose(a: ComposeArgs, out: &mut dyn Write) -> Result<ExitStatus, CliError> {
    engine::check_compression_level(a.compression_level)?;
    image::check_nifti_name(&a.reference, "reference image")?;
    image::check_nifti_name(&a.output, "output field")?;
    let grid = image::grid(&a.reference, "reference image")?;
    let (paths, invert) = transform_paths(&a.transforms);
    let chain = engine::guarded(|| engine::read_chain(&paths, invert.as_deref()))?;
    engine::compose_chain(&chain, &grid, &a.output, a.compression_level)?;
    print(out, &format!("wrote {}\n", a.output.display()))?;
    Ok(ExitStatus::Ok)
}

fn recipe(a: RecipeArgs, out: &mut dyn Write) -> Result<ExitStatus, CliError> {
    print_json(out, &Recipe::cohort(usize::from(a.channels)).to_json())?;
    Ok(ExitStatus::Ok)
}

fn gpu_info(a: GpuInfoArgs, out: &mut dyn Write) -> Result<ExitStatus, CliError> {
    let info = engine::gpu_info(a.adapter)?;
    print_json(
        out,
        &json!({
            "name": info.name,
            "backend": info.backend,
            "device_type": info.device_type,
            "driver": info.driver,
            "memory_budget_bytes": info.memory_budget_bytes,
        }),
    )?;
    Ok(ExitStatus::Ok)
}

fn fits(a: FitsArgs, out: &mut dyn Write) -> Result<ExitStatus, CliError> {
    if let Some(mm) = a.iso_mm {
        engine::check_iso_mm(mm)?;
    }
    image::check_nifti_name(&a.fixed, "fixed image")?;
    if let Some(m) = &a.moving {
        image::check_nifti_name(m, "moving image")?;
    }
    let fixed_grid = image::grid(&a.fixed, "fixed image")?;
    let iso_mm = a
        .iso_mm
        .unwrap_or_else(|| engine::finest_spacing(&fixed_grid));
    let dims = engine::iso_dims(&fixed_grid, iso_mm);
    let moving_dims = a
        .moving
        .as_deref()
        .map(|p| image::grid(p, "moving image").map(|g| engine::iso_dims(&g, iso_mm)))
        .transpose()?;
    let channels = usize::from(a.channels);
    let recipe = load_recipe(a.recipe.as_deref(), channels)?;
    let check = engine::capacity_check(
        dims,
        moving_dims,
        channels,
        a.common.adapter.clone(),
        &recipe,
    )?;
    if a.common.json {
        print_json(
            out,
            &json!({
                "fits": check.is_ok(),
                "grid": dims,
                "moving_grid": moving_dims.unwrap_or(dims),
                "channels": channels,
                "iso_mm": iso_mm,
                "reason": check.as_ref().err(),
            }),
        )?;
    } else {
        let what = format!(
            "{} grid at {} mm, {channels} channel(s)",
            grid_text(dims),
            iso_mm
        );
        match &check {
            Ok(()) => print(out, &format!("fits: {what}\n"))?,
            Err(why) => print(out, &format!("does not fit: {what}: {why}\n"))?,
        }
    }
    Ok(if check.is_ok() {
        ExitStatus::Ok
    } else {
        ExitStatus::GpuCapacity
    })
}
