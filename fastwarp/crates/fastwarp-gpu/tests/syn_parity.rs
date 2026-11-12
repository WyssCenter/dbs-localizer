#![allow(clippy::unwrap_used, clippy::needless_range_loop)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

mod common;

use std::time::Instant;

use common::*;
use fastwarp_gpu::{plan_capacity, plan_capacity_pair, CapacityCaps, WgpuBackend};
use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::backend::Backend;
use fastwarp_reg::cpu::CpuBackend;
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::syn::{run_syn, SynChannel, SynOptions, SynResult};
use fastwarp_reg::{AffineTransform, Volume};

fn channels(p: &Phantom, two: bool) -> Vec<SynChannel> {
    let mut c = vec![SynChannel {
        fixed: p.fixed.clone(),
        moving: p.moving.clone(),
    }];
    if two {
        c.push(SynChannel {
            fixed: p.fixed2.clone(),
            moving: p.moving2.clone(),
        });
    }
    c
}

struct Comparison {
    warp_rms_diff: f64,
    warp_rms: f64,
    inverse_rms_diff: f64,
    ncc_cpu: f64,
    ncc_gpu: f64,
    recovery_cpu: f64,
    recovery_gpu: f64,
    initial_error: f64,
    consistency_cpu: f64,
    consistency_gpu: f64,
    max_energy_diff: f64,
    iterations_cpu: Vec<usize>,
    iterations_gpu: Vec<usize>,
    inversion_steps_cpu: f64,
    inversion_steps_gpu: f64,
    inversion_steps_max_diff: u32,
    cpu_seconds: f64,
    gpu_seconds: f64,
}

fn rms_diff(
    a: &fastwarp_reg::DisplacementField,
    b: &fastwarp_reg::DisplacementField,
) -> (f64, f64) {
    let mut d = 0.0;
    let mut r = 0.0;
    for (x, y) in a.data().iter().zip(b.data()) {
        d += f64::from(x - y).powi(2);
        r += f64::from(*x).powi(2);
    }
    let n = a.data().len() as f64;
    ((d / n).sqrt(), (r / n).sqrt())
}

fn steps(r: &SynResult) -> Vec<u32> {
    r.levels
        .iter()
        .flat_map(|l| {
            l.iterations
                .iter()
                .flat_map(|i| i.inversions.iter().map(|s| s.iterations))
        })
        .collect()
}

fn compare(
    gpu: &WgpuBackend,
    p: &Phantom,
    recipe: &SynRecipe,
    two: bool,
    options: &SynOptions,
) -> Comparison {
    let ch = channels(p, two);
    let t = Instant::now();
    let cpu = run_syn(&CpuBackend, recipe, &ch, &p.initial, options).unwrap();
    let cpu_seconds = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let g = run_syn(gpu, recipe, &ch, &p.initial, options).unwrap();
    let gpu_seconds = t.elapsed().as_secs_f64();
    let (warp_rms_diff, warp_rms) = rms_diff(&cpu.warp, &g.warp);
    let (inverse_rms_diff, _) = rms_diff(&cpu.inverse_warp, &g.inverse_warp);
    let (_, ncc_cpu) = warped_ncc(p, &cpu, &p.moving, &p.fixed);
    let (_, ncc_gpu) = warped_ncc(p, &g, &p.moving, &p.fixed);
    let (initial_error, recovery_cpu, _) = recovery(p, &cpu);
    let (_, recovery_gpu, _) = recovery(p, &g);
    let mut max_energy_diff = 0.0f64;
    for (a, b) in cpu.levels.iter().zip(&g.levels) {
        for (x, y) in a.iterations.iter().zip(&b.iterations) {
            max_energy_diff = max_energy_diff.max((x.energy - y.energy).abs());
        }
    }
    let sc = steps(&cpu);
    let sg = steps(&g);
    let mean = |v: &[u32]| v.iter().sum::<u32>() as f64 / v.len().max(1) as f64;
    let inversion_steps_max_diff = if sc.len() == sg.len() {
        sc.iter()
            .zip(&sg)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0)
    } else {
        u32::MAX
    };
    Comparison {
        warp_rms_diff,
        warp_rms,
        inverse_rms_diff,
        ncc_cpu,
        ncc_gpu,
        recovery_cpu,
        recovery_gpu,
        initial_error,
        consistency_cpu: inverse_consistency(p, &cpu),
        consistency_gpu: inverse_consistency(p, &g),
        max_energy_diff,
        iterations_cpu: cpu.levels.iter().map(|l| l.iterations.len()).collect(),
        iterations_gpu: g.levels.iter().map(|l| l.iterations.len()).collect(),
        inversion_steps_cpu: mean(&sc),
        inversion_steps_gpu: mean(&sg),
        inversion_steps_max_diff,
        cpu_seconds,
        gpu_seconds,
    }
}

fn report(label: &str, c: &Comparison) {
    eprintln!(
        "[{label}] warp RMS diff {:.3e} mm (warp RMS {:.3e}, rel {:.2e}); inverse RMS diff {:.3e} mm",
        c.warp_rms_diff,
        c.warp_rms,
        c.warp_rms_diff / c.warp_rms.max(1e-12),
        c.inverse_rms_diff
    );
    eprintln!(
        "[{label}] NCC after: cpu {:.6} gpu {:.6} (diff {:.2e}); recovery error: initial {:.3} mm, cpu {:.3}, gpu {:.3}; forward o inverse: cpu {:.3} gpu {:.3} voxels",
        c.ncc_cpu,
        c.ncc_gpu,
        (c.ncc_cpu - c.ncc_gpu).abs(),
        c.initial_error,
        c.recovery_cpu,
        c.recovery_gpu,
        c.consistency_cpu,
        c.consistency_gpu
    );
    eprintln!(
        "[{label}] max energy diff {:.3e}; iterations cpu {:?} gpu {:?}; mean inversion steps cpu {:.3} gpu {:.3}, max per-call difference {}; time cpu {:.2} s gpu {:.2} s",
        c.max_energy_diff,
        c.iterations_cpu,
        c.iterations_gpu,
        c.inversion_steps_cpu,
        c.inversion_steps_gpu,
        c.inversion_steps_max_diff,
        c.cpu_seconds,
        c.gpu_seconds
    );
}

#[test]
fn syn_on_the_blob_phantom_matches_the_cpu_reference() {
    let Some(gpu) = backend() else { return };
    let p = phantom(24, 2.0, 2.0);
    let mut recipe = small_recipe(&[(2, 1.0, 20), (1, 0.0, 12)], 2);
    recipe.convergence_threshold = -1.0;
    let c = compare(&gpu, &p, &recipe, false, &SynOptions::default());
    report("blob 24^3 T1", &c);
    assert!(c.warp_rms_diff / c.warp_rms < 0.05, "warp field differs");
    assert!((c.ncc_cpu - c.ncc_gpu).abs() < 2e-3);
    assert!(c.ncc_gpu > 0.97);
    assert!(c.recovery_gpu < 0.5 * c.initial_error);
    assert!(c.recovery_gpu < c.recovery_cpu + 0.05);
    assert!(c.consistency_gpu < 0.25);
    assert!(c.max_energy_diff < 2e-3);
}

#[test]
fn multispectral_syn_matches_the_cpu_reference() {
    let Some(gpu) = backend() else { return };
    let p = phantom(16, 2.0, 1.5);
    let mut recipe = small_recipe(&[(2, 1.0, 12), (1, 0.0, 6)], 2);
    recipe.channel_weights = vec![0.5, 0.5];
    recipe.convergence_threshold = -1.0;
    let c = compare(&gpu, &p, &recipe, true, &SynOptions::default());
    report("blob 16^3 T1+T2", &c);
    assert!(c.warp_rms_diff / c.warp_rms < 0.05);
    assert!((c.ncc_cpu - c.ncc_gpu).abs() < 2e-3);
    assert!(c.max_energy_diff < 2e-3);
}

#[test]
fn the_convergence_monitor_stops_the_same_levels() {
    let Some(gpu) = backend() else { return };
    let p = phantom(16, 2.0, 1.0);
    let mut recipe = small_recipe(&[(2, 1.0, 200)], 2);
    recipe.convergence_threshold = 1e-3;
    let c = compare(&gpu, &p, &recipe, false, &SynOptions::default());
    report("blob 16^3 converge", &c);
    let (a, b) = (c.iterations_cpu[0], c.iterations_gpu[0]);
    assert!(a < 200 && b < 200);
    assert!(a.abs_diff(b) <= 3, "cpu {a} gpu {b} iterations");
}

#[test]
fn gpu_syn_is_deterministic_bit_for_bit() {
    let Some(gpu) = backend() else { return };
    let p = phantom(16, 2.0, 1.5);
    let mut recipe = small_recipe(&[(2, 1.0, 4), (1, 0.0, 3)], 2);
    recipe.channel_weights = vec![0.5, 0.5];
    let ch = channels(&p, true);
    let a = run_syn(&gpu, &recipe, &ch, &p.initial, &SynOptions::default()).unwrap();
    let b = run_syn(&gpu, &recipe, &ch, &p.initial, &SynOptions::default()).unwrap();
    let bits = |r: &SynResult| -> Vec<u32> {
        r.warp
            .data()
            .iter()
            .chain(r.inverse_warp.data())
            .map(|v| v.to_bits())
            .collect()
    };
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(a.levels, b.levels);
}

#[test]
fn gpu_syn_is_deterministic_with_smoothed_total_field_and_wide_kernels() {
    let Some(gpu) = backend() else { return };
    let p = phantom(16, 2.0, 1.5);
    let variants: [(&str, bool, f64, f64); 4] = [
        ("one channel, default smoothing", false, 3.0, 0.0),
        ("two channels, default smoothing", true, 3.0, 0.0),
        ("two channels, smoothed total field", true, 3.0, 2.0),
        ("one channel, wide update kernel", false, 14.0, 6.0),
    ];
    for (label, two, update_variance, total_variance) in variants {
        let mut recipe = small_recipe(&[(2, 1.0, 3), (1, 0.0, 3)], 2);
        if two {
            recipe.channel_weights = vec![0.5, 0.5];
        }
        recipe.update_field_variance = update_variance;
        recipe.total_field_variance = total_variance;
        recipe.convergence_threshold = -1.0;
        let ch = channels(&p, two);
        let a = run_syn(&gpu, &recipe, &ch, &p.initial, &SynOptions::default()).unwrap();
        let b = run_syn(&gpu, &recipe, &ch, &p.initial, &SynOptions::default()).unwrap();
        eprintln!("DIGEST {label}: {:016x}", digest(&a));
        assert_eq!(digest(&a), digest(&b), "{label}");
        assert!(a.warp.data().iter().all(|v| v.is_finite()), "{label}");
    }
}

#[test]
fn a_syn_iteration_reads_back_four_scalar_blocks_with_one_channel_and_five_with_two() {
    let Some(gpu) = backend() else { return };
    let p = phantom(16, 2.0, 1.5);
    for (two, expected) in [(false, 4), (true, 5)] {
        let readbacks = |iterations: u32| {
            let mut recipe = small_recipe(&[(1, 0.0, iterations)], 2);
            if two {
                recipe.channel_weights = vec![0.5, 0.5];
            }
            recipe.convergence_threshold = -1.0;
            let before = gpu.stats().scalar_readbacks;
            let r = run_syn(
                &gpu,
                &recipe,
                &channels(&p, two),
                &p.initial,
                &SynOptions::default(),
            )
            .unwrap();
            assert_eq!(r.levels[0].iterations.len(), iterations as usize);
            gpu.stats().scalar_readbacks - before
        };
        let per_iteration = (readbacks(5) - readbacks(2)) / 3;
        assert_eq!(per_iteration, expected, "two channels: {two}");
    }
}

fn capacity_plan_bounds_pool_peak(n: usize, moving_n: usize, iterations: u32) {
    let n_full = (n * n * n) as u64;
    let mut h = head([n, n, n], 2.0, 3.0);
    if moving_n != n {
        let spacing = (n - 1) as f64 * 2.0 / (moving_n - 1) as f64;
        h.moving = head([moving_n; 3], spacing, 3.0).moving;
    }
    let contrast = |v: &Volume<f64>| {
        Volume::from_vec(*v.grid(), v.data().iter().map(|x| contrast2(*x)).collect()).unwrap()
    };
    let mut failures = Vec::new();
    for channels in [1usize, 2] {
        let Some(gpu) = backend() else { return };
        let mut recipe = small_recipe(&[(2, 1.0, iterations), (1, 0.0, iterations)], 2);
        let mut ch = vec![SynChannel {
            fixed: h.fixed.clone(),
            moving: h.moving.clone(),
        }];
        if channels == 2 {
            recipe.channel_weights = vec![0.5, 0.5];
            ch.push(SynChannel {
                fixed: contrast(&h.fixed),
                moving: contrast(&h.moving),
            });
        }
        recipe.convergence_threshold = -1.0;
        run_syn(
            &gpu,
            &recipe,
            &ch,
            &AffineTransform::identity(),
            &SynOptions::default(),
        )
        .unwrap();
        let peak = gpu.peak_allocated_bytes();
        let caps = CapacityCaps {
            max_storage_buffer_binding_size: u64::MAX,
            memory_budget: u64::MAX,
        };
        let plan = plan_capacity_pair([n; 3], [moving_n; 3], channels, &caps).unwrap();
        eprintln!(
            "channels {channels}: peak {peak} bytes = {:.1} B/voxel, planned {} bytes = {:.1} B/voxel",
            peak as f64 / n_full as f64,
            plan.level_total_bytes,
            plan.level_total_bytes as f64 / n_full as f64
        );
        failures.extend((peak > plan.level_total_bytes + 16 * n_full).then(|| {
            format!(
                "channels {channels}: peak {peak} above plan {} + 16 n",
                plan.level_total_bytes
            )
        }));
        if moving_n != n {
            let fixed_only = plan_capacity([n; 3], channels, &caps).unwrap();
            failures.extend(
                (peak <= fixed_only.level_total_bytes + 16 * n_full).then(|| {
                    format!(
                        "channels {channels}: peak {peak} within the fixed-only plan {} + 16 n",
                        fixed_only.level_total_bytes
                    )
                }),
            );
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn the_capacity_plan_bounds_the_pool_peak_of_a_two_level_run() {
    capacity_plan_bounds_pool_peak(96, 96, 1);
}

#[test]
fn the_capacity_plan_bounds_the_pool_peak_with_a_finer_moving_grid() {
    capacity_plan_bounds_pool_peak(96, 191, 1);
}

#[test]
#[ignore = "recalibration at 128^3; run with --ignored --release"]
fn the_capacity_plan_bounds_the_pool_peak_of_a_two_level_run_at_128() {
    capacity_plan_bounds_pool_peak(128, 128, 3);
}

#[test]
fn gpu_syn_result_applies_through_the_cpu_apply_path() {
    let Some(gpu) = backend() else { return };
    let p = phantom(20, 2.0, 1.5);
    let recipe = small_recipe(&[(2, 1.0, 3), (1, 0.0, 2)], 2);
    let ch = channels(&p, false);
    let g = run_syn(
        &gpu,
        &recipe,
        &ch,
        &AffineTransform::identity(),
        &SynOptions::default(),
    )
    .unwrap();
    let w: Volume<f64> = apply_chain(
        &p.moving,
        &p.grid,
        &g.pair.forward,
        Interpolation::Linear,
        0.0,
    )
    .unwrap();
    assert!(w.data().iter().all(|v| v.is_finite()));
    let name = gpu.name();
    assert!(name.starts_with("wgpu-f32"));
}
