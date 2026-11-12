// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use fastwarp_gpu::ab::{initial_affine_from_composite, load_f64, AbError, AbResult};
use fastwarp_gpu::{BackendChoice, BackendSelect, WgpuBackend};
use fastwarp_reg::linear::{CpuLinearEngine, LinearEngine, LinearStageResult};
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::register::{
    register, register_full, write_linear_output, write_outputs, LinearPlan, RegisterInput,
    RegisterOutput,
};
use fastwarp_reg::syn::SynOptions;
use fastwarp_reg::transform::{read_transform_file, AffineTransform, TransformElem};
use fastwarp_reg::{Backend, Volume};
use serde::Serialize;

/// Linear stages and SyN of the cohort recipe with ANTs-compatible outputs.
#[derive(Parser, Debug)]
#[command(name = "fastwarp_reg_syn")]
struct Cli {
    /// Template T1w.
    #[arg(long)]
    fixed: PathBuf,
    /// Template T2w (second channel).
    #[arg(long)]
    fixed2: Option<PathBuf>,
    /// Moving T1w.
    #[arg(long)]
    moving: PathBuf,
    /// Moving T2w in T1w space (second channel).
    #[arg(long)]
    moving2: Option<PathBuf>,
    /// ANTs Composite.h5 whose element 0 is the initial affine (selects `--linear ants`).
    #[arg(long)]
    initial_composite: Option<PathBuf>,
    /// ITK .mat / .txt affine (maps fixed points to moving points; selects `--linear ants`).
    #[arg(long)]
    initial_affine: Option<PathBuf>,
    /// Linear stages: gpu (centre of mass, Rigid, Affine on the GPU), cpu (same on the f64
    /// reference), ants (take the affine from --initial-composite / --initial-affine). Defaults to
    /// ants when an initial transform is given, gpu otherwise.
    #[arg(long)]
    linear: Option<String>,
    /// Output prefix (ANTs style: Composite.h5 etc. are appended).
    #[arg(long)]
    out: PathBuf,
    /// wgpu or cpu (the SyN stage).
    #[arg(long, default_value = "wgpu")]
    backend: String,
    /// Registration grid spacing in millimetres.
    #[arg(long, default_value_t = 1.0)]
    iso_mm: f64,
    /// SyN iterations per level, e.g. 100x70x50x20.
    #[arg(long)]
    iterations: Option<String>,
    /// Linear iterations per level for both stages, e.g. 1000x500x250x100.
    #[arg(long)]
    linear_iterations: Option<String>,
    /// Write timings as JSON.
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LinearMode {
    Gpu,
    Cpu,
    Ants,
}

#[derive(Serialize)]
struct LevelRecord {
    level: usize,
    grid: [usize; 3],
    iterations: usize,
    converged: bool,
    last_energy: f64,
}

#[derive(Serialize)]
struct LinearLevelRecord {
    level: usize,
    grid: [usize; 3],
    samples: usize,
    iterations: u32,
    converged: bool,
    best_value: f64,
    initial_learning_rate: f64,
    line_search_evaluations: u64,
    seconds: f64,
    parameters: Vec<f64>,
}

#[derive(Serialize)]
struct LinearStageRecord {
    stage: &'static str,
    levels: Vec<LinearLevelRecord>,
}

#[derive(Serialize)]
struct Record {
    backend: String,
    adapter: Option<String>,
    linear: String,
    iso_mm: f64,
    channels: usize,
    resample_s: f64,
    preprocess_s: f64,
    linear_s: f64,
    syn_s: f64,
    warp_s: f64,
    write_s: f64,
    load_s: f64,
    total_s: f64,
    gpu_allocated_bytes: Option<u64>,
    levels: Vec<LevelRecord>,
    linear_stages: Vec<LinearStageRecord>,
    affine_parameters: Option<[f64; 12]>,
    affine_center: Option<[f64; 3]>,
}

fn affine_from_file(path: &Path) -> AbResult<AffineTransform> {
    let chain = read_transform_file(path)?;
    match chain.queue().first() {
        Some(TransformElem::Affine(a)) => Ok(*a),
        _ => Err("initial affine file holds no affine transform".into()),
    }
}

fn parse_levels(text: &str, what: &str, expected: usize) -> AbResult<Vec<u32>> {
    let parsed: Result<Vec<u32>, _> = text.split('x').map(|t| t.trim().parse::<u32>()).collect();
    let parsed = parsed.map_err(|e| -> AbError { format!("bad {what}: {e}").into() })?;
    if parsed.len() != expected {
        return Err(format!("{what} needs {expected} levels").into());
    }
    Ok(parsed)
}

fn recipe(cli: &Cli) -> AbResult<SynRecipe> {
    let mut r = if cli.fixed2.is_some() {
        SynRecipe::cohort_multispectral()
    } else {
        SynRecipe::cohort()
    };
    if let Some(it) = &cli.iterations {
        r.iterations = parse_levels(it, "--iterations", r.iterations.len())?;
    }
    Ok(r)
}

fn plan(cli: &Cli) -> AbResult<LinearPlan> {
    let mut p = LinearPlan::cohort();
    if let Some(it) = &cli.linear_iterations {
        let v = parse_levels(it, "--linear-iterations", p.rigid.iterations.len())?;
        p.rigid.iterations = v.clone();
        p.affine.iterations = v;
    }
    Ok(p)
}

fn linear_mode(cli: &Cli) -> AbResult<LinearMode> {
    let has_initial = cli.initial_composite.is_some() || cli.initial_affine.is_some();
    match cli.linear.as_deref() {
        None if has_initial => Ok(LinearMode::Ants),
        None | Some("gpu") => Ok(LinearMode::Gpu),
        Some("cpu") => Ok(LinearMode::Cpu),
        Some("ants") => {
            if has_initial {
                Ok(LinearMode::Ants)
            } else {
                Err("--linear ants needs --initial-composite or --initial-affine".into())
            }
        }
        Some(other) => Err(format!("unknown --linear {other}, use gpu, cpu or ants").into()),
    }
}

fn stage_record(stage: &'static str, s: &LinearStageResult) -> LinearStageRecord {
    LinearStageRecord {
        stage,
        levels: s
            .levels
            .iter()
            .map(|l| LinearLevelRecord {
                level: l.level,
                grid: l.grid_size,
                samples: l.samples,
                iterations: l.cg.iterations,
                converged: l.cg.converged,
                best_value: l.cg.best_value,
                initial_learning_rate: l.cg.initial_learning_rate,
                line_search_evaluations: l.cg.line_search_evaluations,
                seconds: l.seconds,
                parameters: l.parameters.clone(),
            })
            .collect(),
    }
}

type Channels = (Vec<Volume<f64>>, Vec<Volume<f64>>);

fn load_channels(cli: &Cli) -> AbResult<Channels> {
    let mut fixed = vec![load_f64(&cli.fixed)?];
    let mut moving = vec![load_f64(&cli.moving)?];
    match (&cli.fixed2, &cli.moving2) {
        (Some(f2), Some(m2)) => {
            fixed.push(load_f64(f2)?);
            moving.push(load_f64(m2)?);
        }
        (None, None) => {}
        _ => return Err("--fixed2 and --moving2 go together".into()),
    }
    Ok((fixed, moving))
}

fn full<B: Backend, E: LinearEngine>(
    backend: &B,
    engine: &E,
    cli: &Cli,
    recipe: &SynRecipe,
    fixed: &[Volume<f64>],
    moving: &[Volume<f64>],
) -> AbResult<RegisterOutput> {
    Ok(register_full(
        backend,
        engine,
        &plan(cli)?,
        recipe,
        fixed,
        moving,
        Some(cli.iso_mm),
        &SynOptions::default(),
    )?)
}

fn run_on<B: Backend>(
    backend: &B,
    gpu: Option<&WgpuBackend>,
    cli: &Cli,
    adapter: Option<String>,
) -> AbResult<()> {
    let t_all = Instant::now();
    let recipe = recipe(cli)?;
    let mode = linear_mode(cli)?;
    let t = Instant::now();
    let (fixed, moving) = load_channels(cli)?;
    let load_s = t.elapsed().as_secs_f64();
    eprintln!("loaded {} channel(s) in {load_s:.1} s", fixed.len());
    let out: RegisterOutput = match mode {
        LinearMode::Ants => {
            let initial = match (&cli.initial_composite, &cli.initial_affine) {
                (Some(c), _) => initial_affine_from_composite(c)?,
                (None, Some(a)) => affine_from_file(a)?,
                (None, None) => return Err("give --initial-composite or --initial-affine".into()),
            };
            let input = RegisterInput {
                fixed: &fixed,
                moving: &moving,
                initial,
                iso_mm: Some(cli.iso_mm),
            };
            register(backend, &recipe, &input, &SynOptions::default())?
        }
        LinearMode::Cpu => full(backend, &CpuLinearEngine, cli, &recipe, &fixed, &moving)?,
        LinearMode::Gpu => {
            let engine = gpu.ok_or("the GPU linear engine needs a device")?;
            full(backend, engine, cli, &recipe, &fixed, &moving)?
        }
    };
    drop((fixed, moving));
    let t = Instant::now();
    write_outputs(&cli.out, &out)?;
    if let Some(chain) = &out.linear {
        write_linear_output(&cli.out, &chain.total)?;
    }
    let write_s = t.elapsed().as_secs_f64();
    let total_s = t_all.elapsed().as_secs_f64();
    let tm = out.timings;
    println!("backend            {}", backend.name());
    println!("linear stages      {mode:?}");
    println!("registration grid  {:?}", out.grid.size());
    println!("load               {load_s:8.2} s");
    println!("resample           {:8.2} s", tm.resample_s);
    println!("preprocess         {:8.2} s", tm.preprocess_s);
    println!("linear             {:8.2} s", tm.linear_s);
    println!("syn                {:8.2} s", tm.syn_s);
    println!("warped image       {:8.2} s", tm.warp_s);
    println!("write outputs      {write_s:8.2} s");
    println!("total              {total_s:8.2} s");
    let mut linear_stages = Vec::new();
    if let Some(chain) = &out.linear {
        for (name, stage) in [("rigid", &chain.rigid), ("affine", &chain.affine)] {
            for l in &stage.levels {
                println!(
                    "{name} level {} grid {:?}: {} samples, {} iterations, converged {}, best value {:.6}, {:.1} s",
                    l.level,
                    l.grid_size,
                    l.samples,
                    l.cg.iterations,
                    l.cg.converged,
                    l.cg.best_value,
                    l.seconds
                );
            }
            linear_stages.push(stage_record(name, stage));
        }
    }
    let mut levels = Vec::new();
    for l in &out.levels {
        let last = l.iterations.last().map(|i| i.energy).unwrap_or(0.0);
        println!(
            "level {} grid {:?}: {} iterations, converged {}, last energy {:.6}",
            l.level,
            l.grid_size,
            l.iterations.len(),
            l.converged,
            last
        );
        levels.push(LevelRecord {
            level: l.level,
            grid: l.grid_size,
            iterations: l.iterations.len(),
            converged: l.converged,
            last_energy: last,
        });
    }
    if let Some(path) = &cli.json {
        let rec = Record {
            backend: backend.name(),
            adapter,
            linear: format!("{mode:?}").to_lowercase(),
            iso_mm: cli.iso_mm,
            channels: recipe.channel_weights.len(),
            resample_s: tm.resample_s,
            preprocess_s: tm.preprocess_s,
            linear_s: tm.linear_s,
            syn_s: tm.syn_s,
            warp_s: tm.warp_s,
            write_s,
            load_s,
            total_s,
            gpu_allocated_bytes: gpu.map(WgpuBackend::peak_allocated_bytes),
            levels,
            linear_stages,
            affine_parameters: out.linear.as_ref().map(|c| c.total.parameters()),
            affine_center: out.linear.as_ref().map(|c| c.total.center),
        };
        std::fs::write(path, serde_json::to_string_pretty(&rec)?)?;
    }
    Ok(())
}

fn make_gpu(choice: Option<&str>) -> AbResult<(WgpuBackend, String)> {
    let mut sel = BackendSelect::from_env()?;
    if let Some(other) = choice {
        if other != "wgpu" {
            sel.backend = BackendChoice::parse(other)?;
        }
    }
    let gpu = WgpuBackend::new(&sel)?;
    let adapter = format!(
        "{} ({}, {})",
        gpu.context().info.name,
        gpu.context().info.backend,
        gpu.context().info.device_type
    );
    eprintln!("adapter: {adapter}");
    Ok((gpu, adapter))
}

fn run(cli: &Cli) -> AbResult<()> {
    let mode = linear_mode(cli)?;
    match cli.backend.as_str() {
        "cpu" => {
            if mode == LinearMode::Gpu {
                let (gpu, adapter) = make_gpu(None)?;
                run_on(&fastwarp_reg::CpuBackend, Some(&gpu), cli, Some(adapter))
            } else {
                run_on(&fastwarp_reg::CpuBackend, None, cli, None)
            }
        }
        other => {
            let (gpu, adapter) = make_gpu(Some(other))?;
            run_on(&gpu, Some(&gpu), cli, Some(adapter))
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fastwarp_reg_syn: {e}");
            ExitCode::FAILURE
        }
    }
}
