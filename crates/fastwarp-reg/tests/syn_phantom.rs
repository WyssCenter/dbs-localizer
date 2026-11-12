#![allow(clippy::unwrap_used, clippy::needless_range_loop)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::time::Instant;

use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::backend::Backend;
use fastwarp_reg::cpu::CpuBackend;
use fastwarp_reg::field_ops::VectorField;
use fastwarp_reg::linalg::{Vec3, IDENTITY3};
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::syn::{run_syn, SynChannel, SynOptions, SynResult};
use fastwarp_reg::{AffineTransform, Grid, Volume};

struct Blob {
    centre: Vec3,
    sigma: f64,
    amplitude: f64,
}

fn blobs(centre: Vec3) -> Vec<Blob> {
    let off = |d: Vec3| [centre[0] + d[0], centre[1] + d[1], centre[2] + d[2]];
    vec![
        Blob {
            centre: off([0.0, 0.0, 0.0]),
            sigma: 7.0,
            amplitude: 0.7,
        },
        Blob {
            centre: off([9.0, 3.0, -2.0]),
            sigma: 3.5,
            amplitude: 0.9,
        },
        Blob {
            centre: off([-8.0, 6.0, 4.0]),
            sigma: 4.0,
            amplitude: 0.8,
        },
        Blob {
            centre: off([2.0, -9.0, 5.0]),
            sigma: 3.0,
            amplitude: 0.6,
        },
        Blob {
            centre: off([-3.0, -4.0, -9.0]),
            sigma: 3.5,
            amplitude: 0.85,
        },
    ]
}

fn intensity(blobs: &[Blob], p: Vec3) -> f64 {
    let mut v = 0.0;
    for b in blobs {
        let d2 = (p[0] - b.centre[0]).powi(2)
            + (p[1] - b.centre[1]).powi(2)
            + (p[2] - b.centre[2]).powi(2);
        v += b.amplitude * (-d2 / (2.0 * b.sigma * b.sigma)).exp();
    }
    (v / 1.6).min(1.0)
}

fn contrast2(v: f64) -> f64 {
    1.0 - v * v
}

fn deformation(extent: f64, centre: Vec3, amplitude: f64, p: Vec3) -> Vec3 {
    let k = 2.0 * std::f64::consts::PI / (1.3 * extent);
    [
        amplitude * (k * (p[1] - centre[1])).sin(),
        0.8 * amplitude * (k * (p[2] - centre[2]) + 0.5).sin(),
        0.6 * amplitude * (k * (p[0] - centre[0])).sin(),
    ]
}

struct Phantom {
    grid: Grid,
    fixed: Volume<f64>,
    moving: Volume<f64>,
    fixed2: Volume<f64>,
    moving2: Volume<f64>,
    truth: VectorField,
    initial: AffineTransform,
}

fn phantom(n: usize, spacing: f64, amplitude: f64) -> Phantom {
    let origin = [-(n as f64 - 1.0) * spacing / 2.0; 3];
    let grid = Grid::new([n, n, n], [spacing; 3], origin, IDENTITY3).unwrap();
    let extent = n as f64 * spacing;
    let centre = [0.0; 3];
    let bl = blobs(centre);
    let t0 = [1.0, -0.5, 0.5];
    let initial = AffineTransform::from_translation(t0);
    let map = |p: Vec3| {
        let s = deformation(extent, centre, amplitude, p);
        [p[0] + s[0], p[1] + s[1], p[2] + s[2]]
    };
    let fixed = Volume::from_vec(
        grid,
        (0..grid.num_voxels())
            .map(|idx| {
                let [i, j, k] = grid.ijk(idx);
                intensity(&bl, grid.index_to_point([i as f64, j as f64, k as f64]))
            })
            .collect(),
    )
    .unwrap();
    let moving = Volume::from_vec(
        grid,
        (0..grid.num_voxels())
            .map(|idx| {
                let [i, j, k] = grid.ijk(idx);
                let y = grid.index_to_point([i as f64, j as f64, k as f64]);
                intensity(&bl, map([y[0] - t0[0], y[1] - t0[1], y[2] - t0[2]]))
            })
            .collect(),
    )
    .unwrap();
    let fixed2 =
        Volume::from_vec(grid, fixed.data().iter().map(|v| contrast2(*v)).collect()).unwrap();
    let moving2 =
        Volume::from_vec(grid, moving.data().iter().map(|v| contrast2(*v)).collect()).unwrap();
    let truth = VectorField::from_fn(grid, |x| {
        let mut y = x;
        for _ in 0..60 {
            let s = deformation(extent, centre, amplitude, y);
            y = [x[0] - s[0], x[1] - s[1], x[2] - s[2]];
        }
        [y[0] - x[0], y[1] - x[1], y[2] - x[2]]
    });
    Phantom {
        grid,
        fixed,
        moving,
        fixed2,
        moving2,
        truth,
        initial,
    }
}

fn small_recipe(levels: &[(u32, f64, u32)], radius: usize) -> SynRecipe {
    let mut r = SynRecipe::cohort();
    r.cc_radius = radius;
    r.iterations = levels.iter().map(|l| l.2).collect();
    r.shrink_factors = levels.iter().map(|l| l.0).collect();
    r.smoothing_sigmas = levels.iter().map(|l| l.1).collect();
    r
}

fn ncc(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let ma = a.iter().sum::<f64>() / n;
    let mb = b.iter().sum::<f64>() / n;
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa * sbb).sqrt()
}

struct Recovery {
    initial_error: f64,
    final_error: f64,
    points: usize,
}

fn recovery(p: &Phantom, result: &SynResult) -> Recovery {
    let g = p.grid;
    let [nx, ny, nz] = g.size();
    let margin = 3;
    let (mut e0, mut e1, mut count) = (0.0, 0.0, 0usize);
    for idx in 0..g.num_voxels() {
        let [i, j, k] = g.ijk(idx);
        if i < margin
            || j < margin
            || k < margin
            || i + margin >= nx
            || j + margin >= ny
            || k + margin >= nz
        {
            continue;
        }
        if p.fixed.data()[idx] < 0.15 {
            continue;
        }
        let x = g.index_to_point([i as f64, j as f64, k as f64]);
        let truth = p.truth.data()[idx];
        let rec = result.pair.forward.apply(x);
        let want = p
            .initial
            .apply([x[0] + truth[0], x[1] + truth[1], x[2] + truth[2]]);
        let dev =
            ((rec[0] - want[0]).powi(2) + (rec[1] - want[1]).powi(2) + (rec[2] - want[2]).powi(2))
                .sqrt();
        let init = (truth[0].powi(2) + truth[1].powi(2) + truth[2].powi(2)).sqrt();
        e0 += init;
        e1 += dev;
        count += 1;
    }
    Recovery {
        initial_error: e0 / count as f64,
        final_error: e1 / count as f64,
        points: count,
    }
}

fn warped_ncc(
    p: &Phantom,
    result: &SynResult,
    moving: &Volume<f64>,
    fixed: &Volume<f64>,
) -> (f64, f64) {
    let before = {
        let chain =
            fastwarp_reg::TransformChain::from_queue(vec![fastwarp_reg::TransformElem::Affine(
                p.initial,
            )]);
        let w = apply_chain(moving, &p.grid, &chain, Interpolation::Linear, 0.0).unwrap();
        ncc(fixed.data(), w.data())
    };
    let w = apply_chain(
        moving,
        &p.grid,
        &result.pair.forward,
        Interpolation::Linear,
        0.0,
    )
    .unwrap();
    (before, ncc(fixed.data(), w.data()))
}

fn run(p: &Phantom, recipe: &SynRecipe, two_channels: bool, options: &SynOptions) -> SynResult {
    let mut channels = vec![SynChannel {
        fixed: p.fixed.clone(),
        moving: p.moving.clone(),
    }];
    if two_channels {
        channels.push(SynChannel {
            fixed: p.fixed2.clone(),
            moving: p.moving2.clone(),
        });
    }
    run_syn(&CpuBackend, recipe, &channels, &p.initial, options).unwrap()
}

fn inverse_consistency(p: &Phantom, result: &SynResult) -> f64 {
    let g = p.grid;
    let [nx, ny, nz] = g.size();
    let margin = 3;
    let mut worst = 0.0f64;
    for idx in 0..g.num_voxels() {
        let [i, j, k] = g.ijk(idx);
        if i < margin
            || j < margin
            || k < margin
            || i + margin >= nx
            || j + margin >= ny
            || k + margin >= nz
        {
            continue;
        }
        let x = g.index_to_point([i as f64, j as f64, k as f64]);
        let y = result.pair.forward.apply(x);
        let back = result.pair.inverse.apply(y);
        let d =
            ((back[0] - x[0]).powi(2) + (back[1] - x[1]).powi(2) + (back[2] - x[2]).powi(2)).sqrt();
        worst = worst.max(d / g.spacing()[0]);
    }
    worst
}

#[test]
fn syn_recovers_a_known_smooth_deformation_on_a_blob_phantom() {
    let p = phantom(24, 2.0, 2.0);
    let mut recipe = small_recipe(&[(2, 1.0, 25), (1, 0.0, 15)], 2);
    recipe.convergence_threshold = -1.0;
    let t = Instant::now();
    let result = run(&p, &recipe, false, &SynOptions::default());
    let elapsed = t.elapsed();
    let (before, after) = warped_ncc(&p, &result, &p.moving, &p.fixed);
    let rec = recovery(&p, &result);
    let consistency = inverse_consistency(&p, &result);
    let inv_steps: Vec<u32> = result
        .levels
        .iter()
        .flat_map(|l| {
            l.iterations
                .iter()
                .flat_map(|i| i.inversions.iter().map(|s| s.iterations))
        })
        .collect();
    let mean_steps = inv_steps.iter().sum::<u32>() as f64 / inv_steps.len() as f64;
    eprintln!(
        "phantom: {:.1?}, NCC {before:.4} -> {after:.4}, mean |w - w_true| {:.3} mm -> {:.3} mm over {} voxels, forward o inverse {consistency:.3} voxels, mean inversion steps {mean_steps:.2}, iterations {:?}",
        elapsed,
        rec.initial_error,
        rec.final_error,
        rec.points,
        result.levels.iter().map(|l| l.iterations.len()).collect::<Vec<_>>()
    );
    assert!(after > before + 0.02, "NCC {before} -> {after}");
    assert!(after > 0.97, "NCC after {after}");
    assert!(rec.points > 500);
    assert!(
        rec.final_error < 0.5 * rec.initial_error,
        "recovery {} -> {}",
        rec.initial_error,
        rec.final_error
    );
    assert!(consistency < 0.25, "forward o inverse {consistency} voxels");
    assert_eq!(result.warp.grid().size(), [24, 24, 24]);
}

#[test]
fn syn_is_deterministic_bit_for_bit() {
    let p = phantom(16, 2.0, 1.5);
    let mut recipe = small_recipe(&[(2, 1.0, 4), (1, 0.0, 3)], 2);
    recipe.channel_weights = vec![0.5, 0.5];
    let a = run(&p, &recipe, true, &SynOptions::default());
    let b = run(&p, &recipe, true, &SynOptions::default());
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
fn multispectral_run_improves_both_channels_and_the_monitor_uses_the_first_metric() {
    let p = phantom(16, 2.0, 1.5);
    let mut recipe = small_recipe(&[(2, 1.0, 12), (1, 0.0, 6)], 2);
    recipe.channel_weights = vec![0.5, 0.5];
    recipe.convergence_threshold = -1.0;
    let first = run(&p, &recipe, true, &SynOptions::default());
    let (b1, a1) = warped_ncc(&p, &first, &p.moving, &p.fixed);
    let (b2, a2) = warped_ncc(&p, &first, &p.moving2, &p.fixed2);
    assert!(a1 > b1 && a2 > b2, "{b1} {a1} {b2} {a2}");
    let mut differs = false;
    for level in &first.levels {
        for i in &level.iterations {
            assert_eq!(i.energy.to_bits(), i.energy_first_metric.to_bits());
            if (i.energy_first_metric - i.energy_weighted).abs() > 1e-9 {
                differs = true;
            }
        }
    }
    assert!(differs);
}

#[test]
fn a_single_channel_ignores_the_weight_and_both_energies_agree() {
    let p = phantom(16, 2.0, 1.5);
    let mut recipe = small_recipe(&[(2, 1.0, 3)], 2);
    recipe.channel_weights = vec![0.25];
    let a = run(&p, &recipe, false, &SynOptions::default());
    recipe.channel_weights = vec![7.0];
    let b = run(&p, &recipe, false, &SynOptions::default());
    assert_eq!(a.warp, b.warp);
    for i in &a.levels[0].iterations {
        assert_eq!(i.energy_first_metric.to_bits(), i.energy_weighted.to_bits());
    }
}

#[test]
fn the_monitor_stops_a_level_when_the_energy_flattens() {
    let p = phantom(16, 2.0, 1.0);
    let mut recipe = small_recipe(&[(2, 1.0, 200)], 2);
    recipe.convergence_threshold = 1e-3;
    let result = run(&p, &recipe, false, &SynOptions::default());
    let level = &result.levels[0];
    assert!(level.converged);
    assert!(level.iterations.len() >= 10 && level.iterations.len() < 200);
    let last = level.iterations.last().unwrap();
    assert!(last.convergence_value < 1e-3);
    for it in &level.iterations[..level.iterations.len() - 1] {
        assert!(it.convergence_value >= 1e-3);
    }
}

#[test]
fn one_iteration_moves_the_largest_voxel_by_about_the_gradient_step_per_side() {
    let p = phantom(16, 2.0, 1.5);
    let mut recipe = small_recipe(&[(1, 0.0, 1)], 2);
    recipe.convergence_threshold = -1.0;
    let backend = CpuBackend;
    let result = run(&p, &recipe, false, &SynOptions::default());
    let it = &result.levels[0].iterations;
    assert_eq!(it.len(), 1);
    assert!(it[0].fixed_update_scale > 0.0 && it[0].moving_update_scale > 0.0);
    let warp = backend
        .upload_field(&VectorField::from_displacement_field(&result.warp))
        .unwrap();
    let largest = backend.max_scaled_norm(&warp).unwrap();
    assert!(largest > 0.05, "{largest}");
    assert!(largest <= 2.0 * recipe.gradient_step * 1.05, "{largest}");
}

#[test]
fn results_do_not_depend_on_the_number_of_threads() {
    let p = phantom(16, 2.0, 1.5);
    let mut recipe = small_recipe(&[(2, 1.0, 3), (1, 0.0, 2)], 2);
    recipe.channel_weights = vec![0.5, 0.5];
    let bits = |r: &SynResult| -> Vec<u32> {
        r.warp
            .data()
            .iter()
            .chain(r.inverse_warp.data())
            .map(|v| v.to_bits())
            .collect()
    };
    let run_in = |threads: usize| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| run(&p, &recipe, true, &SynOptions::default()))
    };
    let one = run_in(1);
    let three = run_in(3);
    assert_eq!(bits(&one), bits(&three));
    assert_eq!(one.levels, three.levels);
}

fn head_intensity(p: Vec3, radii: Vec3) -> f64 {
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
    0.3 * scalp + 0.9 * skull + brain * (0.55 - 0.35 * vent + 0.25 * thal)
}

#[test]
#[ignore = "larger synthetic head at 3 mm; prints timing and inversion step counts, run on demand"]
fn syn_on_a_synthetic_head_at_three_millimetres() {
    let spacing = 3.0;
    let size = [56usize, 66, 56];
    let origin = [
        -(size[0] as f64 - 1.0) * spacing / 2.0,
        -(size[1] as f64 - 1.0) * spacing / 2.0,
        -(size[2] as f64 - 1.0) * spacing / 2.0,
    ];
    let grid = Grid::new(size, [spacing; 3], origin, IDENTITY3).unwrap();
    let radii = [70.0, 90.0, 75.0];
    let extent = 170.0;
    let amplitude = 4.0;
    let map = |q: Vec3| {
        let s = deformation(extent, [0.0; 3], amplitude, q);
        [q[0] + s[0], q[1] + s[1], q[2] + s[2]]
    };
    let make = |f: &dyn Fn(Vec3) -> f64| {
        Volume::from_vec(
            grid,
            (0..grid.num_voxels())
                .map(|idx| {
                    let [i, j, k] = grid.ijk(idx);
                    f(grid.index_to_point([i as f64, j as f64, k as f64]))
                })
                .collect(),
        )
        .unwrap()
    };
    let fixed = make(&|p| head_intensity(p, radii));
    let moving = make(&|p| head_intensity(map(p), radii));
    let mut recipe = SynRecipe::cohort();
    recipe.iterations = vec![30, 20, 10];
    recipe.shrink_factors = vec![4, 2, 1];
    recipe.smoothing_sigmas = vec![2.0, 1.0, 0.0];
    let channels = vec![SynChannel {
        fixed: fixed.clone(),
        moving: moving.clone(),
    }];
    let t = Instant::now();
    let result = run_syn(
        &CpuBackend,
        &recipe,
        &channels,
        &AffineTransform::identity(),
        &SynOptions::default(),
    )
    .unwrap();
    let elapsed = t.elapsed();
    let before = ncc(fixed.data(), moving.data());
    let warped = apply_chain(
        &moving,
        &grid,
        &result.pair.forward,
        Interpolation::Linear,
        0.0,
    )
    .unwrap();
    let after = ncc(fixed.data(), warped.data());
    eprintln!("head 3 mm {size:?}: {elapsed:.1?}, NCC {before:.4} -> {after:.4}");
    for l in &result.levels {
        let steps: Vec<u32> = l
            .iterations
            .iter()
            .flat_map(|i| i.inversions.iter().map(|s| s.iterations))
            .collect();
        let mean = steps.iter().sum::<u32>() as f64 / steps.len().max(1) as f64;
        let max = steps.iter().max().copied().unwrap_or(0);
        eprintln!(
            "level {} grid {:?}: {} iterations, converged {}, inversion steps mean {mean:.2} max {max}",
            l.level,
            l.grid_size,
            l.iterations.len(),
            l.converged
        );
    }
    assert!(after > before);
}
