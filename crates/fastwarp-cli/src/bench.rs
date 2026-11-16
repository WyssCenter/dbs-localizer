// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! `fastwarp bench`: times the registration (the default recipe, the `register` path without
//! writing files) on deterministic synthetic head volumes at standard sizes, or on two images,
//! and reports the machine it ran on. Nothing is downloaded and ANTs is not needed.

use std::io::Write;
#[cfg(any(windows, target_os = "macos"))]
use std::path::Path;
#[cfg(any(windows, target_os = "macos"))]
use std::process::Command;
use std::time::Instant;

use clap::ValueEnum;
use fastwarp_reg::linalg::IDENTITY3;
use fastwarp_reg::{Grid, Vec3, Volume};
use rayon::prelude::*;
use serde_json::{json, Value};

use crate::args::BenchArgs;
use crate::commands::{load_recipe, print, print_json};
use crate::engine::{self, GpuInfo};
use crate::image;
use crate::recipe::Recipe;
use crate::{CliError, ExitStatus};

/// Field of view of the synthetic volumes in millimetres, that of the MNI152 1 mm grid.
const FOV_MM: [f64; 3] = [182.0, 218.0, 182.0];

/// A synthetic volume size.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// 30x36x30 voxels at 6 mm, for tests.
    #[value(name = "tiny")]
    Tiny,
    /// 91x109x91 voxels at 2 mm.
    #[value(name = "mni-2mm")]
    Mni2mm,
    /// 182x218x182 voxels at 1 mm.
    #[value(name = "mni-1mm")]
    Mni1mm,
    /// 364x436x364 voxels at 0.5 mm.
    #[value(name = "mni-0.5mm")]
    Mni05mm,
}

impl Size {
    /// The sizes run when none is given.
    pub const DEFAULT: [Size; 2] = [Size::Mni1mm, Size::Mni05mm];

    pub fn name(self) -> &'static str {
        match self {
            Size::Tiny => "tiny",
            Size::Mni2mm => "mni-2mm",
            Size::Mni1mm => "mni-1mm",
            Size::Mni05mm => "mni-0.5mm",
        }
    }

    pub fn spacing(self) -> f64 {
        match self {
            Size::Tiny => 6.0,
            Size::Mni2mm => 2.0,
            Size::Mni1mm => 1.0,
            Size::Mni05mm => 0.5,
        }
    }

    pub fn dims(self) -> [usize; 3] {
        let s = self.spacing();
        let n = |a: usize| (FOV_MM[a] / s).round() as usize;
        [n(0), n(1), n(2)]
    }
}

fn head_intensity(p: Vec3) -> f64 {
    let radii = [70.0, 90.0, 75.0];
    let r =
        ((p[0] / radii[0]).powi(2) + (p[1] / radii[1]).powi(2) + (p[2] / radii[2]).powi(2)).sqrt();
    let edge = |x: f64, w: f64| 0.5 * (1.0 + (x / w).tanh());
    let scalp = edge(1.0 - r, 0.03);
    let skull = edge(0.92 - r, 0.02) - edge(0.85 - r, 0.02);
    let brain = edge(0.85 - r, 0.02);
    let vent =
        (-(((p[0] / 8.0).powi(2)) + ((p[1] + 6.0) / 22.0).powi(2) + ((p[2] - 8.0) / 9.0).powi(2)))
            .exp();
    let thal =
        (-(((p[0] - 15.0) / 10.0).powi(2) + (p[1] / 12.0).powi(2) + (p[2] / 10.0).powi(2))).exp();
    let folds = 0.04 * (0.31 * p[0]).sin() * (0.27 * p[1]).sin() * (0.23 * p[2]).sin();
    0.3 * scalp + 0.9 * skull + brain * (0.55 - 0.35 * vent + 0.25 * thal + folds)
}

fn deformation(p: Vec3) -> Vec3 {
    let amplitude = 3.0;
    let k = 2.0 * std::f64::consts::PI / (1.3 * 170.0);
    [
        amplitude * (k * p[1]).sin(),
        0.8 * amplitude * (k * p[2] + 0.5).sin(),
        0.6 * amplitude * (k * p[0]).sin(),
    ]
}

/// The rigid offset of the moving phantom: 4 degrees about z, 2 about x, then a shift in mm.
fn offset(p: Vec3) -> Vec3 {
    let (sz, cz) = 4f64.to_radians().sin_cos();
    let (sx, cx) = 2f64.to_radians().sin_cos();
    let q = [cz * p[0] - sz * p[1], sz * p[0] + cz * p[1], p[2]];
    let q = [q[0], cx * q[1] - sx * q[2], sx * q[1] + cx * q[2]];
    [q[0] + 3.0, q[1] - 4.0, q[2] + 2.0]
}

/// The synthetic fixed and moving volumes of `size`: a head phantom, and the same phantom
/// deformed smoothly and moved rigidly. Deterministic, so runs on any machine are comparable.
pub fn phantom(size: Size) -> Result<(Volume<f64>, Volume<f64>), CliError> {
    let dims = size.dims();
    let s = size.spacing();
    let origin = [
        -(dims[0] as f64 - 1.0) * s / 2.0,
        -(dims[1] as f64 - 1.0) * s / 2.0,
        -(dims[2] as f64 - 1.0) * s / 2.0,
    ];
    let grid = Grid::new(dims, [s; 3], origin, IDENTITY3)
        .map_err(|e| CliError::new(ExitStatus::Registration, e.to_string()))?;
    let make = |f: &(dyn Fn(Vec3) -> f64 + Sync)| {
        let data: Vec<f64> = (0..grid.num_voxels())
            .into_par_iter()
            .map(|idx| {
                let [i, j, k] = grid.ijk(idx);
                f(grid.index_to_point([i as f64, j as f64, k as f64]))
            })
            .collect();
        Volume::from_vec(grid, data)
            .map_err(|e| CliError::new(ExitStatus::Registration, e.to_string()))
    };
    let fixed = make(&head_intensity)?;
    let moving = make(&|p: Vec3| {
        let q = offset(p);
        let d = deformation(q);
        head_intensity([q[0] + d[0], q[1] + d[1], q[2] + d[2]])
    })?;
    Ok((fixed, moving))
}

/// The timings of one run in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunTimes {
    pub linear_s: f64,
    pub syn_s: f64,
    /// The registration's phases (resampling, preprocessing, linear, SyN, final warp).
    pub total_s: f64,
    /// Around the whole call, including opening the GPU and the capacity check.
    pub wall_s: f64,
}

/// One benchmarked case.
#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub name: String,
    pub grid: [usize; 3],
    pub iso_mm: f64,
    pub skipped: Option<String>,
    /// Why the registration failed; the other cases still run.
    pub failed: Option<String>,
    pub runs: Vec<RunTimes>,
}

/// The machine a benchmark ran on.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub gpu: String,
    pub driver: String,
    pub backend: String,
    pub device_type: String,
    pub memory_budget_bytes: u64,
    pub cpu: String,
    pub logical_cores: usize,
    pub threads: usize,
    pub threads_source: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub fastwarp_version: String,
}

/// A whole benchmark.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub machine: Machine,
    pub warmup: u32,
    pub repeat: u32,
    pub cases: Vec<Case>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// The report as JSON.
pub fn report_json(r: &Report) -> Value {
    let m = &r.machine;
    json!({
        "fastwarp_version": m.fastwarp_version,
        "machine": {
            "gpu": m.gpu,
            "driver": m.driver,
            "backend": m.backend,
            "device_type": m.device_type,
            "memory_budget_bytes": m.memory_budget_bytes,
            "cpu": m.cpu,
            "logical_cores": m.logical_cores,
            "threads": m.threads,
            "threads_source": m.threads_source,
            "os": m.os,
            "os_version": m.os_version,
            "arch": m.arch,
        },
        "warmup": r.warmup,
        "repeat": r.repeat,
        "cases": r.cases.iter().map(|c| json!({
            "name": c.name,
            "grid": c.grid,
            "iso_mm": c.iso_mm,
            "status": if c.skipped.is_some() {
                "skipped"
            } else if c.failed.is_some() {
                "failed"
            } else {
                "ok"
            },
            "reason": c.skipped.as_ref().or(c.failed.as_ref()),
            "runs": c.runs.iter().map(|t| json!({
                "linear_s": t.linear_s,
                "syn_s": t.syn_s,
                "total_s": t.total_s,
                "wall_s": t.wall_s,
            })).collect::<Vec<_>>(),
            "median_total_s": median(c.runs.iter().map(|t| t.total_s).collect()),
            "median_wall_s": median(c.runs.iter().map(|t| t.wall_s).collect()),
        })).collect::<Vec<_>>(),
    })
}

/// The report as text.
pub fn report_text(r: &Report) -> String {
    let m = &r.machine;
    let mut s = format!("fastwarp {} bench\n", m.fastwarp_version);
    s.push_str(&format!(
        "gpu            {} ({}, {}), driver {}\n",
        m.gpu,
        m.backend,
        m.device_type,
        if m.driver.is_empty() {
            "unknown"
        } else {
            &m.driver
        }
    ));
    s.push_str(&format!("memory budget  {}\n", gib(m.memory_budget_bytes)));
    s.push_str(&format!(
        "cpu            {}, {} logical cores, {} threads ({})\n",
        m.cpu, m.logical_cores, m.threads, m.threads_source
    ));
    s.push_str(&format!(
        "os             {} {} {}\n",
        m.os, m.os_version, m.arch
    ));
    if r.warmup > 0 {
        s.push_str(&format!(
            "warm-up        {} run(s) per case, not shown\n",
            r.warmup
        ));
    }
    s.push_str(&format!(
        "\n{:<11} {:<13} {:>3}  {:>9} {:>9} {:>9} {:>9}\n",
        "case", "grid", "run", "linear_s", "syn_s", "total_s", "wall_s"
    ));
    for c in &r.cases {
        let grid = format!("{}x{}x{}", c.grid[0], c.grid[1], c.grid[2]);
        if let Some(why) = &c.skipped {
            s.push_str(&format!("{:<11} {:<13} skipped: {why}\n", c.name, grid));
            continue;
        }
        if let Some(why) = &c.failed {
            s.push_str(&format!("{:<11} {:<13} failed: {why}\n", c.name, grid));
            continue;
        }
        for (i, t) in c.runs.iter().enumerate() {
            s.push_str(&format!(
                "{:<11} {:<13} {:>3}  {:>9.2} {:>9.2} {:>9.2} {:>9.2}\n",
                c.name,
                grid,
                i + 1,
                t.linear_s,
                t.syn_s,
                t.total_s,
                t.wall_s
            ));
        }
        if c.runs.len() > 1 {
            let med = |f: fn(&RunTimes) -> f64| {
                median(c.runs.iter().map(f).collect()).unwrap_or(f64::NAN)
            };
            s.push_str(&format!(
                "{:<11} {:<13} {:>3}  {:>9.2} {:>9.2} {:>9.2} {:>9.2}\n",
                c.name,
                grid,
                "med",
                med(|t| t.linear_s),
                med(|t| t.syn_s),
                med(|t| t.total_s),
                med(|t| t.wall_s)
            ));
        }
    }
    s
}

#[cfg(any(windows, target_os = "macos"))]
fn command_output(program: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(windows)]
fn registry_value(key: &str, name: &str) -> Option<String> {
    let root = std::env::var_os("SystemRoot")?;
    let reg = Path::new(&root).join("System32").join("reg.exe");
    let text = command_output(&reg, &["query", key, "/v", name])?;
    text.lines()
        .find(|l| l.trim_start().starts_with(name))
        .and_then(|l| l.split_once("REG_SZ"))
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The CPU model, as the operating system names it.
pub fn cpu_model() -> String {
    #[cfg(windows)]
    {
        registry_value(
            r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "ProcessorNameString",
        )
        .or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
        .unwrap_or_else(|| "unknown".into())
    }
    #[cfg(target_os = "macos")]
    {
        command_output(
            Path::new("/usr/sbin/sysctl"),
            &["-n", "machdep.cpu.brand_string"],
        )
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find(|l| l.starts_with("model name") || l.starts_with("Model"))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim().to_string())
            })
            .unwrap_or_else(|| "unknown".into())
    }
}

/// The operating system version, where it is cheap to find.
pub fn os_version() -> String {
    #[cfg(windows)]
    {
        registry_value(
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "CurrentBuild",
        )
        .map(|b| format!("build {b}"))
        .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        command_output(Path::new("/usr/bin/sw_vers"), &["-productVersion"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .unwrap_or_default()
    }
}

/// Sets the size of the global thread pool before anything starts it.
fn set_threads(threads: Option<u32>) -> Result<String, CliError> {
    if let Some(n) = threads {
        let n = n as usize;
        if rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
            .is_err()
            && rayon::current_num_threads() != n
        {
            return Err(CliError::usage(format!(
                "--threads {n}: the thread pool already runs with {} threads in this process",
                rayon::current_num_threads()
            )));
        }
        return Ok("--threads".into());
    }
    let from_env = std::env::var("RAYON_NUM_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .is_some_and(|n| n > 0);
    Ok(if from_env {
        "RAYON_NUM_THREADS".into()
    } else {
        "all cores".into()
    })
}

fn machine(gpu: &GpuInfo, threads_source: String) -> Machine {
    Machine {
        gpu: gpu.name.clone(),
        driver: gpu.driver.clone(),
        backend: gpu.backend.clone(),
        device_type: gpu.device_type.clone(),
        memory_budget_bytes: gpu.memory_budget_bytes,
        cpu: cpu_model(),
        logical_cores: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        threads: rayon::current_num_threads(),
        threads_source,
        os: std::env::consts::OS.into(),
        os_version: os_version(),
        arch: std::env::consts::ARCH.into(),
        fastwarp_version: env!("CARGO_PKG_VERSION").into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn time_case(
    name: &str,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    iso_mm: f64,
    recipe: &Recipe,
    adapter: &Option<String>,
    warmup: u32,
    repeat: u32,
    err: &mut dyn Write,
) -> Result<Vec<RunTimes>, CliError> {
    let mut runs = Vec::new();
    for i in 0..warmup + repeat {
        let label = if i < warmup {
            format!("warm-up {}", i + 1)
        } else {
            format!("run {}", i - warmup + 1)
        };
        let _ = writeln!(err, "fastwarp bench: {name}: {label}");
        let (f, m) = (fixed.clone(), moving.clone());
        let t = Instant::now();
        let run = engine::register_syn(
            vec![f],
            vec![m],
            None,
            iso_mm,
            adapter.clone(),
            None,
            recipe.clone(),
        )?;
        let wall_s = t.elapsed().as_secs_f64();
        if i >= warmup {
            let p = &run.timings;
            runs.push(RunTimes {
                linear_s: p.linear_s,
                syn_s: p.syn_s,
                total_s: p.resample_s + p.preprocess_s + p.linear_s + p.syn_s + p.warp_s,
                wall_s,
            });
        }
    }
    Ok(runs)
}

/// Stores the runs of a case, or why its registration failed; other errors stop the benchmark.
fn record(case: &mut Case, runs: Result<Vec<RunTimes>, CliError>) -> Result<(), CliError> {
    match runs {
        Ok(r) => case.runs = r,
        Err(e) if e.status == ExitStatus::Registration => case.failed = Some(e.message),
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Runs `fastwarp bench`.
pub fn run(a: BenchArgs, out: &mut dyn Write, err: &mut dyn Write) -> Result<ExitStatus, CliError> {
    let threads_source = set_threads(a.threads)?;
    let warmup = a.warmup.unwrap_or(if a.repeat > 1 { 1 } else { 0 });
    let recipe = load_recipe(a.recipe.as_deref(), 1)?;
    let adapter = a.common.adapter.clone();
    let user = match (&a.fixed, &a.moving) {
        (Some(f), Some(m)) => {
            if let Some(mm) = a.iso_mm {
                engine::check_iso_mm(mm)?;
            }
            image::check_nifti_name(f, "fixed image")?;
            image::check_nifti_name(m, "moving image")?;
            Some((f.clone(), m.clone()))
        }
        _ => None,
    };
    let gpu = engine::gpu_info(adapter.clone())?;
    let machine = machine(&gpu, threads_source);
    let mut cases = Vec::new();
    if let Some((f, m)) = user {
        let fixed = image::load(&f, "fixed image")?;
        let moving = image::load(&m, "moving image")?;
        let iso_mm = a
            .iso_mm
            .unwrap_or_else(|| engine::finest_spacing(fixed.grid()));
        let dims = engine::iso_dims(fixed.grid(), iso_mm);
        let moving_dims = engine::iso_dims(moving.grid(), iso_mm);
        let mut case = Case {
            name: "images".into(),
            grid: dims,
            iso_mm,
            skipped: None,
            failed: None,
            runs: Vec::new(),
        };
        match engine::capacity_check(dims, Some(moving_dims), 1, adapter.clone(), &recipe)? {
            Err(why) => case.skipped = Some(format!("does not fit the GPU: {why}")),
            Ok(()) => {
                let runs = time_case(
                    "images", &fixed, &moving, iso_mm, &recipe, &adapter, warmup, a.repeat, err,
                );
                record(&mut case, runs)?;
            }
        }
        cases.push(case);
    } else {
        let sizes: Vec<Size> = if a.sizes.is_empty() {
            Size::DEFAULT.to_vec()
        } else {
            a.sizes.clone()
        };
        for size in sizes {
            let mut case = Case {
                name: size.name().into(),
                grid: size.dims(),
                iso_mm: size.spacing(),
                skipped: None,
                failed: None,
                runs: Vec::new(),
            };
            match engine::capacity_check(size.dims(), None, 1, adapter.clone(), &recipe)? {
                Err(why) => case.skipped = Some(format!("does not fit the GPU: {why}")),
                Ok(()) => {
                    let (fixed, moving) = phantom(size)?;
                    let runs = time_case(
                        size.name(),
                        &fixed,
                        &moving,
                        size.spacing(),
                        &recipe,
                        &adapter,
                        warmup,
                        a.repeat,
                        err,
                    );
                    record(&mut case, runs)?;
                }
            }
            cases.push(case);
        }
    }
    let any_failed = cases.iter().any(|c| c.failed.is_some());
    let all_skipped = cases.iter().all(|c| c.skipped.is_some());
    let report = Report {
        machine,
        warmup,
        repeat: a.repeat,
        cases,
    };
    if a.common.json {
        print_json(out, &report_json(&report))?;
    } else {
        print(out, &report_text(&report))?;
    }
    Ok(if any_failed {
        ExitStatus::Registration
    } else if all_skipped {
        ExitStatus::GpuCapacity
    } else {
        ExitStatus::Ok
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report {
            machine: Machine {
                gpu: "Test GPU".into(),
                driver: "test driver".into(),
                backend: "Vulkan".into(),
                device_type: "DiscreteGpu".into(),
                memory_budget_bytes: 4 << 30,
                cpu: "Test CPU".into(),
                logical_cores: 8,
                threads: 8,
                threads_source: "all cores".into(),
                os: "linux".into(),
                os_version: "Test OS 1".into(),
                arch: "x86_64".into(),
                fastwarp_version: "0.0.0".into(),
            },
            warmup: 1,
            repeat: 2,
            cases: vec![
                Case {
                    name: "mni-1mm".into(),
                    grid: [182, 218, 182],
                    iso_mm: 1.0,
                    skipped: None,
                    failed: None,
                    runs: vec![
                        RunTimes {
                            linear_s: 1.0,
                            syn_s: 4.0,
                            total_s: 5.5,
                            wall_s: 6.0,
                        },
                        RunTimes {
                            linear_s: 3.0,
                            syn_s: 6.0,
                            total_s: 9.5,
                            wall_s: 10.0,
                        },
                    ],
                },
                Case {
                    name: "mni-0.5mm".into(),
                    grid: [364, 436, 364],
                    iso_mm: 0.5,
                    skipped: Some("does not fit the GPU: too big".into()),
                    failed: None,
                    runs: vec![],
                },
                Case {
                    name: "mni-2mm".into(),
                    grid: [91, 109, 91],
                    iso_mm: 2.0,
                    skipped: None,
                    failed: Some("diverged".into()),
                    runs: vec![],
                },
            ],
        }
    }

    #[test]
    fn sizes_have_the_mni_grids() {
        assert_eq!(Size::Mni1mm.dims(), [182, 218, 182]);
        assert_eq!(Size::Mni2mm.dims(), [91, 109, 91]);
        assert_eq!(Size::Mni05mm.dims(), [364, 436, 364]);
        assert_eq!(Size::Tiny.dims(), [30, 36, 30]);
        for s in Size::value_variants() {
            assert_eq!(Size::from_str(s.name(), false), Ok(*s));
        }
    }

    #[test]
    fn the_phantom_is_deterministic_and_the_pair_differs() {
        let (f1, m1) = phantom(Size::Tiny).unwrap();
        let (f2, m2) = phantom(Size::Tiny).unwrap();
        assert_eq!(f1.data(), f2.data());
        assert_eq!(m1.data(), m2.data());
        assert_eq!(f1.grid().size(), [30, 36, 30]);
        let diff: f64 = f1
            .data()
            .iter()
            .zip(m1.data())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(diff > 1.0);
        let centre = f1.grid().point_to_index([0.0; 3]);
        assert!((centre[0] - 14.5).abs() < 1e-9);
    }

    #[test]
    fn the_json_report_has_every_field() {
        let v = report_json(&sample());
        for key in [
            "gpu",
            "driver",
            "backend",
            "device_type",
            "memory_budget_bytes",
            "cpu",
            "logical_cores",
            "threads",
            "threads_source",
            "os",
            "os_version",
            "arch",
        ] {
            assert!(v["machine"].get(key).is_some(), "{key}");
        }
        assert_eq!(v["fastwarp_version"], "0.0.0");
        assert_eq!(v["cases"][0]["status"], "ok");
        assert_eq!(v["cases"][0]["runs"][1]["wall_s"], 10.0);
        assert_eq!(v["cases"][0]["median_wall_s"], 8.0);
        assert_eq!(v["cases"][1]["status"], "skipped");
        assert_eq!(v["cases"][1]["median_wall_s"], Value::Null);
        assert_eq!(v["cases"][2]["status"], "failed");
        assert_eq!(v["cases"][2]["reason"], "diverged");
    }

    #[test]
    fn the_text_report_shows_runs_medians_and_skips() {
        let t = report_text(&sample());
        assert!(t.starts_with("fastwarp 0.0.0 bench\n"), "{t}");
        assert!(
            t.contains("Test GPU (Vulkan, DiscreteGpu), driver test driver"),
            "{t}"
        );
        assert!(t.contains("memory budget  4.0 GiB"), "{t}");
        assert!(
            t.contains("Test CPU, 8 logical cores, 8 threads (all cores)"),
            "{t}"
        );
        assert!(t.contains("warm-up        1 run(s) per case"), "{t}");
        assert!(
            t.contains("mni-1mm     182x218x182     1       1.00      4.00      5.50      6.00"),
            "{t}"
        );
        assert!(
            t.contains("med       2.00      5.00      7.50      8.00"),
            "{t}"
        );
        assert!(
            t.contains("mni-0.5mm   364x436x364   skipped: does not fit the GPU: too big"),
            "{t}"
        );
        assert!(
            t.contains("mni-2mm     91x109x91     failed: diverged"),
            "{t}"
        );
    }

    #[test]
    fn machine_strings_are_found() {
        assert!(!cpu_model().is_empty());
        let _ = os_version();
    }
}
