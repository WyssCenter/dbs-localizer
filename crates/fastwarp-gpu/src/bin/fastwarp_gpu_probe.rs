// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::process::ExitCode;

use clap::Parser;
use fastwarp_gpu::probe::{self, ProbeOptions, ProbeSet, Tolerances};
use fastwarp_gpu::reference::Grid;
use fastwarp_gpu::{BackendChoice, BackendSelect, GpuContext, GpuError};

/// wgpu go/no-go probe for the GPU registration engine (WP1).
#[derive(Parser, Debug)]
#[command(name = "fastwarp_gpu_probe")]
struct Cli {
    /// Probes to run: all, or a comma-separated list of 1 2 3 4 5.
    #[arg(long, default_value = "all", value_delimiter = ',')]
    probe: Vec<String>,

    /// Backend: auto, vulkan, dx12 or metal (default: FASTWARP_GPU_BACKEND or auto).
    #[arg(long)]
    backend: Option<String>,

    /// Adapter name substring (default: FASTWARP_GPU_ADAPTER).
    #[arg(long)]
    adapter: Option<String>,

    /// Timed repeats per probe.
    #[arg(long, default_value_t = 7)]
    repeat: usize,

    /// Write the full report as JSON.
    #[arg(long)]
    json: Option<std::path::PathBuf>,

    /// Grid as NXxNYxNZ (default 197x233x189).
    #[arg(long)]
    dims: Option<String>,

    /// Box-sum radius of probe 2.
    #[arg(long, default_value_t = 4)]
    radius: u32,

    /// Time with the wall clock instead of GPU timestamps.
    #[arg(long)]
    wall: bool,

    /// Record every kernel of probe 2 in its own compute pass.
    #[arg(long)]
    pass_per_kernel: bool,

    /// Untimed, unverified executions of the probe 2 chain before the verified one.
    #[arg(long, default_value_t = 0)]
    warmup: usize,

    /// Allow software adapters.
    #[arg(long)]
    allow_software: bool,
}

fn parse_dims(s: &str) -> Result<Grid, GpuError> {
    let parts: Vec<&str> = s.split(['x', 'X']).collect();
    if parts.len() != 3 {
        return Err(GpuError::Invalid(format!("bad --dims '{s}'")));
    }
    let p = |t: &str| {
        t.trim()
            .parse::<u32>()
            .map_err(|e| GpuError::Invalid(format!("bad --dims '{s}': {e}")))
    };
    Ok(Grid {
        nx: p(parts[0])?,
        ny: p(parts[1])?,
        nz: p(parts[2])?,
    })
}

fn run(cli: &Cli) -> Result<bool, GpuError> {
    let mut sel = BackendSelect::from_env()?;
    if let Some(b) = &cli.backend {
        sel.backend = BackendChoice::parse(b)?;
    }
    if let Some(a) = &cli.adapter {
        sel.adapter_substring = Some(a.clone());
    }
    if cli.allow_software {
        sel.allow_software = true;
    }
    let mut set = ProbeSet {
        p1: false,
        p2: false,
        p3: false,
        p4: false,
        p5: false,
    };
    for p in &cli.probe {
        match p.trim() {
            "all" => set = ProbeSet::all(),
            "1" => set.p1 = true,
            "2" => set.p2 = true,
            "3" => set.p3 = true,
            "4" => set.p4 = true,
            "5" => set.p5 = true,
            other => return Err(GpuError::Invalid(format!("unknown probe '{other}'"))),
        }
    }
    let mut opt = ProbeOptions {
        repeat: cli.repeat,
        radius: cli.radius,
        force_wall: cli.wall,
        pass_per_kernel: cli.pass_per_kernel,
        warmup_runs: cli.warmup,
        ..ProbeOptions::default()
    };
    if let Some(d) = &cli.dims {
        opt.grid = parse_dims(d)?;
    }
    let ctx = GpuContext::new(&sel)?;
    eprintln!(
        "selected adapter: {} ({}, {})",
        ctx.info.name, ctx.info.backend, ctx.info.device_type
    );
    let mut progress = |m: &str| eprintln!("{m}");
    let report = probe::run_probes(&ctx, set, opt, Tolerances::default(), &mut progress)?;
    println!("{}", probe::render(&report));
    if let Some(path) = &cli.json {
        let text = serde_json::to_string_pretty(&report)
            .map_err(|e| GpuError::Invalid(format!("json: {e}")))?;
        std::fs::write(path, text)
            .map_err(|e| GpuError::Invalid(format!("write {}: {e}", path.display())))?;
    }
    Ok(report.all_correct)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("fastwarp_gpu_probe: correctness check FAILED");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("fastwarp_gpu_probe: {e}");
            ExitCode::from(1)
        }
    }
}
