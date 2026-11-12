#![allow(clippy::unwrap_used, clippy::needless_range_loop)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::backend;
use fastwarp_reg::grid::Grid;
use fastwarp_reg::linalg::{Vec3, IDENTITY3};
use fastwarp_reg::linear::{
    prepare_samples, run_linear_stage, CpuLinearEngine, LinearEngine, LinearKind, LinearRecipe,
};
use fastwarp_reg::mask::MaskVolume;
use fastwarp_reg::optimizer::LinearMetric;
use fastwarp_reg::pyramid::{level_virtual_grid, smooth_for_level, SigmaUnits};
use fastwarp_reg::sampling::{metric_sample_points, SamplingSpec, SamplingStrategy, SeedSequence};
use fastwarp_reg::transform::{AffineTransform, LinearMovingTransform, ParametricTransform};
use fastwarp_reg::Volume;

fn scene(n: usize, spacing: f64, pose: &AffineTransform) -> Volume<f64> {
    let g = Grid::new([n, n, n], [spacing; 3], [0.0; 3], IDENTITY3).unwrap();
    let c = (n as f64 - 1.0) * spacing / 2.0;
    let blobs: [(Vec3, f64, f64); 5] = [
        ([0.0, 0.0, 0.0], 9.0, 0.7),
        ([10.0, 4.0, -3.0], 4.5, 0.9),
        ([-9.0, 7.0, 5.0], 5.0, 0.8),
        ([3.0, -10.0, 6.0], 4.0, 0.6),
        ([-4.0, -5.0, -10.0], 4.5, 0.85),
    ];
    let inv = pose.inverse().unwrap();
    let data = (0..g.num_voxels())
        .map(|lin| {
            let [i, j, k] = g.ijk(lin);
            let p = g.index_to_point([i as f64, j as f64, k as f64]);
            let q = inv.apply(p);
            blobs
                .iter()
                .map(|(b, s, a)| {
                    let d2 = (q[0] - c - b[0]).powi(2)
                        + (q[1] - c - b[1]).powi(2)
                        + (q[2] - c - b[2]).powi(2);
                    a * (-d2 / (2.0 * s * s)).exp()
                })
                .sum::<f64>()
                / 1.6
        })
        .collect();
    Volume::from_vec(g, data).unwrap()
}

struct Level {
    fixed: Volume<f64>,
    moving: Volume<f64>,
    samples: fastwarp_reg::linear::PreparedSamples,
}

fn level(n: usize, sigma: f64, shrink: u32) -> Level {
    let pose = AffineTransform::new(
        [[0.99, 0.02, 0.0], [-0.02, 1.01, 0.01], [0.0, 0.0, 1.0]],
        [1.5, -1.0, 0.7],
        [0.0; 3],
    );
    let fixed = scene(n, 1.0, &AffineTransform::identity());
    let moving = scene(n, 1.0, &pose);
    let grid = level_virtual_grid(fixed.grid(), [shrink; 3]).unwrap();
    let fixed_s = smooth_for_level(&fixed, sigma, SigmaUnits::Voxels).unwrap();
    let moving_s = smooth_for_level(&moving, sigma, SigmaUnits::Voxels).unwrap();
    let raw = metric_sample_points(
        &grid,
        SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage: 0.25,
        },
        &mut SeedSequence::new(20_260_910),
        None,
    );
    let samples = prepare_samples(&fixed_s, &moving_s, &raw, 32).unwrap();
    Level {
        fixed: fixed_s,
        moving: moving_s,
        samples,
    }
}

fn level_with_mask(n: usize, sigma: f64, shrink: u32) -> Level {
    let pose = AffineTransform::new(
        [[0.99, 0.02, 0.0], [-0.02, 1.01, 0.01], [0.0, 0.0, 1.0]],
        [1.5, -1.0, 0.7],
        [0.0; 3],
    );
    let fixed = scene(n, 1.0, &AffineTransform::identity());
    let moving = scene(n, 1.0, &pose);
    let grid = level_virtual_grid(fixed.grid(), [shrink; 3]).unwrap();
    let fixed_s = smooth_for_level(&fixed, sigma, SigmaUnits::Voxels).unwrap();
    let moving_s = smooth_for_level(&moving, sigma, SigmaUnits::Voxels).unwrap();
    let fg = *fixed.grid();
    let half = fg.size()[0] / 2;
    let mask = MaskVolume::from_volume(
        Volume::from_vec(
            fg,
            (0..fg.num_voxels())
                .map(|lin| f64::from(u8::from(fg.ijk(lin)[0] >= half)))
                .collect(),
        )
        .unwrap(),
    );
    let keep = |p: Vec3| mask.is_inside(p);
    let raw = metric_sample_points(
        &grid,
        SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage: 0.25,
        },
        &mut SeedSequence::new(20_260_910),
        Some(&keep),
    );
    let samples = prepare_samples(&fixed_s, &moving_s, &raw, 32).unwrap();
    Level {
        fixed: fixed_s,
        moving: moving_s,
        samples,
    }
}

fn rigid(params: [f64; 6]) -> LinearMovingTransform {
    LinearMovingTransform {
        outer: AffineTransform::from_translation([0.8, -0.4, 0.3]),
        optimised: ParametricTransform::euler3d(params, [24.0, 25.0, 23.0]),
    }
}

fn affine(delta: f64) -> LinearMovingTransform {
    let a = AffineTransform::new(
        [
            [1.0 + delta, 0.01, -0.01],
            [0.0, 1.0 - delta, 0.02],
            [0.01, 0.0, 1.0 + 0.5 * delta],
        ],
        [0.6, -0.5, 0.4],
        [24.0, 25.0, 23.0],
    );
    LinearMovingTransform {
        outer: AffineTransform::new(
            [[0.98, 0.1, 0.0], [-0.1, 0.98, 0.0], [0.0, 0.0, 1.0]],
            [1.0, 0.5, -0.5],
            [0.0; 3],
        ),
        optimised: ParametricTransform::affine(&a),
    }
}

fn compare(t: &LinearMovingTransform, cpu: &impl LinearMetric, gpu: &impl LinearMetric) {
    let (cv, cd) = cpu.value_and_derivative(t).unwrap();
    let (gv, gd) = gpu.value_and_derivative(t).unwrap();
    let value_only = gpu.value(t).unwrap();
    assert_eq!(value_only, gv, "value-only and value-and-derivative differ");
    let rel = (cv - gv).abs() / cv.abs().max(1e-12);
    assert!(rel < 1e-5, "value cpu {cv} gpu {gv} rel {rel}");
    let (mut num, mut den) = (0.0, 0.0);
    for (a, b) in cd.iter().zip(&gd) {
        num += (a - b) * (a - b);
        den += a * a;
    }
    let drel = (num / den.max(1e-30)).sqrt();
    eprintln!("  value rel {rel:.2e}, derivative rel {drel:.2e}");
    assert!(drel < 5e-4, "derivative cpu {cd:?} gpu {gd:?} rel {drel}");
    let (gv2, gd2) = gpu.value_and_derivative(t).unwrap();
    assert_eq!(gv, gv2);
    assert_eq!(gd, gd2, "GPU derivative is not deterministic");
}

#[test]
fn mi_value_and_derivative_match_the_cpu_reference() {
    let Some(be) = backend() else { return };
    for (n, sigma, shrink) in [(48usize, 1.5, 2u32), (48, 0.0, 1), (40, 2.0, 4)] {
        let l = level(n, sigma, shrink);
        eprintln!(
            "n {n} sigma {sigma} shrink {shrink}: {} samples",
            l.samples.points.len()
        );
        let cpu = CpuLinearEngine
            .prepare_level(l.fixed.clone(), l.moving.clone(), l.samples.clone())
            .unwrap();
        let gpu = be
            .prepare_level(l.fixed.clone(), l.moving.clone(), l.samples.clone())
            .unwrap();
        compare(&rigid([0.0; 6]), &cpu, &gpu);
        compare(&rigid([0.02, -0.03, 0.04, 0.5, -0.7, 0.3]), &cpu, &gpu);
        compare(&affine(0.0), &cpu, &gpu);
        compare(&affine(0.03), &cpu, &gpu);
    }
}

#[test]
fn the_histogram_value_does_not_depend_on_the_chunking_and_has_a_stable_digest() {
    let Some(be) = backend() else { return };
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |bits: u64| digest = (digest ^ bits).wrapping_mul(0x0100_0000_01b3);
    for (n, sigma, shrink) in [(48usize, 1.5, 2u32), (48, 0.0, 1)] {
        let l = level(n, sigma, shrink);
        for t in [
            rigid([0.0; 6]),
            rigid([0.02, -0.03, 0.04, 0.5, -0.7, 0.3]),
            affine(0.03),
        ] {
            let mut values = Vec::new();
            for chunk in [256u32, 512, 2048, 4096] {
                let gpu = be
                    .prepare_level_with_chunk(l.moving.clone(), l.samples.clone(), chunk)
                    .unwrap();
                let (value, derivative) = gpu.value_and_derivative(&t).unwrap();
                assert_eq!(value.to_bits(), gpu.value(&t).unwrap().to_bits());
                values.push(value.to_bits());
                if chunk == 2048 {
                    mix(value.to_bits());
                    derivative.iter().for_each(|d| mix(d.to_bits()));
                }
            }
            assert!(
                values.windows(2).all(|w| w[0] == w[1]),
                "n {n}: the value changed with the chunk length: {values:x?}"
            );
        }
    }
    eprintln!("MI DIGEST {digest:016x}");
}

#[test]
fn a_chunk_that_could_overflow_the_workgroup_histogram_is_rejected() {
    let Some(be) = backend() else { return };
    let l = level(48, 1.5, 2);
    for chunk in [0u32, 300, 4352, 6144, 8192] {
        assert!(
            be.prepare_level_with_chunk(l.moving.clone(), l.samples.clone(), chunk)
                .is_err(),
            "chunk {chunk}"
        );
    }
}

#[test]
fn masked_mi_matches_the_cpu_reference_and_is_deterministic() {
    let Some(be) = backend() else { return };
    let masked = level_with_mask(48, 1.5, 2);
    let plain = level(48, 1.5, 2);
    eprintln!(
        "masked {} samples vs plain {}",
        masked.samples.points.len(),
        plain.samples.points.len()
    );
    assert!(!masked.samples.points.is_empty());
    assert!(masked.samples.points.len() < plain.samples.points.len());
    let cpu = CpuLinearEngine
        .prepare_level(
            masked.fixed.clone(),
            masked.moving.clone(),
            masked.samples.clone(),
        )
        .unwrap();
    let gpu = be
        .prepare_level(
            masked.fixed.clone(),
            masked.moving.clone(),
            masked.samples.clone(),
        )
        .unwrap();
    compare(&rigid([0.01, -0.02, 0.03, 0.4, -0.5, 0.2]), &cpu, &gpu);
    compare(&affine(0.02), &cpu, &gpu);
}

#[test]
fn samples_outside_the_moving_image_are_dropped_like_on_the_cpu() {
    let Some(be) = backend() else { return };
    let l = level(40, 1.0, 1);
    let cpu = CpuLinearEngine
        .prepare_level(l.fixed.clone(), l.moving.clone(), l.samples.clone())
        .unwrap();
    let gpu = be
        .prepare_level(l.fixed.clone(), l.moving.clone(), l.samples.clone())
        .unwrap();
    let t = LinearMovingTransform {
        outer: AffineTransform::from_translation([14.0, -12.0, 9.0]),
        optimised: ParametricTransform::euler3d([0.0; 6], [20.0; 3]),
    };
    let (cv, _) = cpu.value_and_derivative(&t).unwrap();
    let (gv, _) = gpu.value_and_derivative(&t).unwrap();
    assert!((cv - gv).abs() / cv.abs() < 5e-3, "{cv} {gv}");
    let far = LinearMovingTransform {
        outer: AffineTransform::from_translation([500.0, 0.0, 0.0]),
        optimised: ParametricTransform::euler3d([0.0; 6], [20.0; 3]),
    };
    assert!(cpu.value(&far).is_err());
    assert!(gpu.value(&far).is_err());
}

#[test]
fn the_rigid_and_affine_stages_agree_between_cpu_and_gpu() {
    let Some(be) = backend() else { return };
    let truth = AffineTransform::new(
        ParametricTransform::euler3d([0.03, -0.02, 0.05, 0.0, 0.0, 0.0], [0.0; 3])
            .to_affine()
            .matrix,
        [2.0, -1.5, 1.0],
        [0.0; 3],
    );
    let fixed = scene(32, 1.0, &AffineTransform::identity());
    let moving = scene(32, 1.0, &truth);
    for kind in [LinearKind::Rigid, LinearKind::Affine] {
        let recipe = LinearRecipe {
            iterations: vec![15, 10, 5],
            shrink_factors: vec![4, 2, 1],
            smoothing_sigmas: vec![2.0, 1.0, 0.0],
            kind,
            ..match kind {
                LinearKind::Rigid => LinearRecipe::cohort_rigid(),
                LinearKind::Affine => LinearRecipe::cohort_affine(),
            }
        };
        let start = match kind {
            LinearKind::Rigid => ParametricTransform::euler3d([0.0; 6], [15.5; 3]),
            LinearKind::Affine => {
                ParametricTransform::affine(&AffineTransform::new(IDENTITY3, [0.0; 3], [15.5; 3]))
            }
        };
        let cpu = run_linear_stage(
            &CpuLinearEngine,
            &recipe,
            &fixed,
            &moving,
            AffineTransform::identity(),
            start,
        )
        .unwrap();
        let gpu = run_linear_stage(
            &be,
            &recipe,
            &fixed,
            &moving,
            AffineTransform::identity(),
            start,
        )
        .unwrap();
        let mut worst: f64 = 0.0;
        for p in [[7.0, 7.0, 7.0], [25.0, 9.0, 22.0], [15.0, 25.0, 10.0]] {
            let (a, b) = (cpu.affine.apply(p), gpu.affine.apply(p));
            let e = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            worst = worst.max(e);
        }
        eprintln!("{kind:?}: worst cpu-vs-gpu point difference {worst:.4} mm");
        assert!(worst < 0.3, "{kind:?}: {worst}");
        for p in [[7.0, 7.0, 7.0], [25.0, 9.0, 22.0]] {
            let (a, b) = (gpu.affine.apply(p), truth.apply(p));
            let e = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            assert!(e < 0.8, "{kind:?} vs truth: {e}");
        }
    }
}

#[test]
fn the_gpu_linear_chain_is_bit_identical_from_run_to_run() {
    use fastwarp_reg::linear::run_linear_chain;
    let Some(be) = backend() else { return };
    let fixed = scene(40, 1.0, &AffineTransform::identity());
    let moving = scene(
        40,
        1.0,
        &AffineTransform::from_translation([2.0, -1.0, 1.5]),
    );
    let recipe = |kind: LinearKind| LinearRecipe {
        iterations: vec![20, 15, 10],
        shrink_factors: vec![4, 2, 1],
        smoothing_sigmas: vec![2.0, 1.0, 0.0],
        kind,
        ..match kind {
            LinearKind::Rigid => LinearRecipe::cohort_rigid(),
            LinearKind::Affine => LinearRecipe::cohort_affine(),
        }
    };
    let run = || {
        run_linear_chain(
            &be,
            &recipe(LinearKind::Rigid),
            &recipe(LinearKind::Affine),
            &fixed,
            &moving,
            &fixed,
            &moving,
        )
        .unwrap()
    };
    let a = run();
    let b = run();
    assert_eq!(a.total.parameters(), b.total.parameters());
    assert_eq!(a.total.center, b.total.center);
    let (ra, rb) = (&a.rigid.levels, &b.rigid.levels);
    for (x, y) in ra.iter().zip(rb) {
        assert_eq!(x.cg.values, y.cg.values);
    }
}
