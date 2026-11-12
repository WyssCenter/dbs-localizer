#![allow(clippy::unwrap_used, clippy::needless_range_loop)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

mod common;

use std::time::Instant;

use common::*;
use fastwarp_gpu::{ProfileReport, WgpuBackend};
use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::cpu::CpuBackend;
use fastwarp_reg::pyramid::smooth_for_level;
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::syn::{run_syn, SynChannel, SynOptions, SynResult};
use fastwarp_reg::AffineTransform;

fn split_levels(report: &ProfileReport, levels: usize) -> Vec<f64> {
    let mut sums = vec![0.0; levels];
    let mut level = 0usize;
    let mut prev = "";
    for (op, s) in &report.timeline {
        if op == "resample_field" && prev != "resample_field" && level + 1 < levels {
            level += 1;
        }
        sums[level] += s;
        prev = op;
    }
    sums
}

fn finest_level_ops(report: &ProfileReport, levels: usize) -> Vec<(String, u64, f64)> {
    let mut level = 0usize;
    let mut prev = "";
    let mut map: std::collections::BTreeMap<String, (u64, f64)> = std::collections::BTreeMap::new();
    for (op, s) in &report.timeline {
        if op == "resample_field" && prev != "resample_field" && level + 1 < levels {
            level += 1;
        }
        prev = op;
        if level + 1 == levels {
            let e = map.entry(op.clone()).or_insert((0, 0.0));
            e.0 += 1;
            e.1 += s;
        }
    }
    let mut v: Vec<(String, u64, f64)> = map.into_iter().map(|(k, (c, s))| (k, c, s)).collect();
    v.sort_by(|a, b| b.2.total_cmp(&a.2));
    v
}

fn print_profile(label: &str, report: &ProfileReport, result: &SynResult) {
    let levels = result.levels.len();
    let per_level = split_levels(report, levels);
    eprintln!("[{label}] GPU time per level (sum of per-operation GPU timestamps):");
    for (l, t) in result.levels.iter().zip(&per_level) {
        let it = l.iterations.len().max(1);
        eprintln!(
            "  level {} grid {:?}: {} iterations, {:.3} s, {:.1} ms per iteration",
            l.level,
            l.grid_size,
            l.iterations.len(),
            t,
            1e3 * t / it as f64
        );
    }
    let finest = finest_level_ops(report, levels);
    let finest_total: f64 = finest.iter().map(|o| o.2).sum();
    eprintln!("[{label}] finest level only (total {finest_total:.3} s):");
    for (op, calls, secs) in &finest {
        eprintln!(
            "  {:<22} {:>6} calls {:>9.3} s {:>8.3} ms/call {:>5.1} %",
            op,
            calls,
            secs,
            1e3 * secs / (*calls).max(1) as f64,
            100.0 * secs / finest_total.max(1e-12)
        );
    }
    let total: f64 = report.ops.iter().map(|o| o.seconds).sum();
    eprintln!("[{label}] GPU time per operation, all levels (total {total:.3} s):");
    for o in &report.ops {
        eprintln!(
            "  {:<22} {:>6} calls {:>9.3} s {:>8.3} ms/call {:>5.1} %",
            o.op,
            o.calls,
            o.seconds,
            1e3 * o.seconds / o.calls.max(1) as f64,
            100.0 * o.seconds / total.max(1e-12)
        );
    }
}

fn host_smoothing_seconds(
    fixed: &fastwarp_reg::Volume<f64>,
    moving: &fastwarp_reg::Volume<f64>,
    recipe: &SynRecipe,
) -> f64 {
    let t = Instant::now();
    for l in 0..recipe.levels() {
        let sigma = recipe.smoothing_sigmas[l];
        let _ = smooth_for_level(fixed, sigma, recipe.sigma_units).unwrap();
        let _ = smooth_for_level(moving, sigma, recipe.sigma_units).unwrap();
    }
    t.elapsed().as_secs_f64()
}

fn bench(gpu: &WgpuBackend, label: &str, h: &Head, recipe: &SynRecipe, with_cpu: bool) {
    let channels = vec![SynChannel {
        fixed: h.fixed.clone(),
        moving: h.moving.clone(),
    }];
    let identity = AffineTransform::identity();
    let options = SynOptions::default();
    let host_smooth = host_smoothing_seconds(&h.fixed, &h.moving, recipe);

    gpu.set_profiling(false);
    let before = gpu.stats();
    let t = Instant::now();
    let result = run_syn(gpu, recipe, &channels, &identity, &options).unwrap();
    let wall = t.elapsed().as_secs_f64();
    let after = gpu.stats();
    let iterations: usize = result.levels.iter().map(|l| l.iterations.len()).sum();
    let steps: Vec<u32> = result
        .levels
        .iter()
        .flat_map(|l| {
            l.iterations
                .iter()
                .flat_map(|i| i.inversions.iter().map(|s| s.iterations))
        })
        .collect();
    let mean_steps = steps.iter().sum::<u32>() as f64 / steps.len().max(1) as f64;
    eprintln!(
        "[{label}] wall time of run_syn (GPU loop + host smoothing + uploads): {wall:.2} s; host-side smoothing alone {host_smooth:.2} s; GPU loop estimate {:.2} s",
        wall - host_smooth
    );
    eprintln!(
        "[{label}] {iterations} iterations in {} levels, mean inversion steps {mean_steps:.2}; submits {}, scalar read-backs {}, uploads {}, field downloads {}, device memory peak {:.3} GB, at end {:.3} GB",
        result.levels.len(),
        after.submits - before.submits,
        after.scalar_readbacks - before.scalar_readbacks,
        after.uploads - before.uploads,
        after.field_downloads - before.field_downloads,
        gpu.peak_allocated_bytes() as f64 / 1e9,
        gpu.allocated_bytes() as f64 / 1e9
    );
    eprintln!("[{label}] DIGEST {:016x}", digest(&result));
    let warped = apply_chain(
        &h.moving,
        &h.grid,
        &result.pair.forward,
        Interpolation::Linear,
        0.0,
    )
    .unwrap();
    eprintln!(
        "[{label}] NCC {:.4} -> {:.4}",
        ncc(h.fixed.data(), h.moving.data()),
        ncc(h.fixed.data(), warped.data())
    );

    gpu.set_profiling(true);
    let _ = gpu.take_profile();
    let profiled = run_syn(gpu, recipe, &channels, &identity, &options).unwrap();
    let report = gpu.take_profile();
    gpu.set_profiling(false);
    print_profile(label, &report, &profiled);

    if with_cpu {
        let t = Instant::now();
        let cpu = run_syn(&CpuBackend, recipe, &channels, &identity, &options).unwrap();
        let cpu_wall = t.elapsed().as_secs_f64();
        let w = apply_chain(
            &h.moving,
            &h.grid,
            &cpu.pair.forward,
            Interpolation::Linear,
            0.0,
        )
        .unwrap();
        let mut d = 0.0;
        let mut r = 0.0;
        for (a, b) in cpu.warp.data().iter().zip(result.warp.data()) {
            d += f64::from(a - b).powi(2);
            r += f64::from(*a).powi(2);
        }
        eprintln!(
            "[{label}] CPU f64 reference: {cpu_wall:.1} s, NCC {:.4}; warp RMS difference GPU vs CPU {:.3e} mm (CPU warp RMS {:.3e})",
            ncc(h.fixed.data(), w.data()),
            (d / cpu.warp.data().len() as f64 * 3.0).sqrt(),
            (r / cpu.warp.data().len() as f64 * 3.0).sqrt()
        );
    }
}

fn bench_enabled() -> bool {
    let on = fastwarp_gpu::device::env_var_os("FASTWARP_GPU_BENCH").is_some();
    if !on {
        eprintln!("skip: set FASTWARP_GPU_BENCH=1 to run the GPU benchmarks");
    }
    on
}

#[test]
#[ignore = "release-mode benchmark on the 3 mm synthetic head; prints per-level and per-operation GPU time"]
fn syn_benchmark_synthetic_head_three_millimetres() {
    if !bench_enabled() {
        return;
    }
    let Some(gpu) = backend() else { return };
    let h = head([56, 66, 56], 3.0, 4.0);
    let mut recipe = SynRecipe::cohort();
    recipe.iterations = vec![30, 20, 10];
    recipe.shrink_factors = vec![4, 2, 1];
    recipe.smoothing_sigmas = vec![2.0, 1.0, 0.0];
    bench(&gpu, "head 3 mm 56x66x56", &h, &recipe, true);
}

#[test]
#[ignore = "release-mode benchmark on a 197x233x189 synthetic volume (1.0 mm template size); needs about 3 GB of RAM"]
fn syn_benchmark_one_millimetre_template_size() {
    if !bench_enabled() {
        return;
    }
    let Some(gpu) = backend() else { return };
    let h = head([197, 233, 189], 1.0, 4.0);
    let recipe = SynRecipe::cohort();
    bench(&gpu, "head 1.0 mm 197x233x189", &h, &recipe, false);
}
