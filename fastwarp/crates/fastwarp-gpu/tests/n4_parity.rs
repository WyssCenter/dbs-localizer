#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! GPU N4 against the `f64` CPU reference. Skips without a usable adapter.

use fastwarp_gpu::{BackendSelect, GpuError, GpuN4};
use fastwarp_reg::grid::Grid;
use fastwarp_reg::linalg::IDENTITY3;
use fastwarp_reg::n4::{run_n4, N4Params, N4_SPLINE_ORDER};
use fastwarp_reg::volume::Volume;

fn require_adapter() -> bool {
    fastwarp_gpu::device::env_var("FASTWARP_GPU_REQUIRE_ADAPTER")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn gpu() -> Option<GpuN4> {
    match GpuN4::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(g) => {
            eprintln!(
                "using adapter {} ({})",
                g.context().info.name,
                g.context().info.backend
            );
            Some(g)
        }
        Err(GpuError::NoAdapter(why)) => {
            if require_adapter() {
                panic!("no GPU adapter with FASTWARP_GPU_REQUIRE_ADAPTER set: {why}");
            }
            eprintln!("skip: no adapter ({why})");
            None
        }
        Err(e) => panic!("device creation failed: {e}"),
    }
}

fn phantom(size: [usize; 3], spacing: f64) -> Volume<f64> {
    let origin = [
        -(size[0] as f64 - 1.0) * spacing / 2.0,
        -(size[1] as f64 - 1.0) * spacing / 2.0,
        -(size[2] as f64 - 1.0) * spacing / 2.0,
    ];
    let g = Grid::new(size, [spacing; 3], origin, IDENTITY3).unwrap();
    let data: Vec<f64> = (0..g.num_voxels())
        .map(|lin| {
            let [i, j, k] = g.ijk(lin);
            let p = g.index_to_point([i as f64, j as f64, k as f64]);
            let r = ((p[0] / 22.0).powi(2) + (p[1] / 22.0).powi(2) + (p[2] / 16.0).powi(2)).sqrt();
            if r >= 1.0 {
                0.0
            } else {
                let edge = 0.5 * (1.0 + ((1.0 - r) / 0.05).tanh());
                edge * (0.5 + 0.3 * (1.0 - r) + 0.1 * (p[0] / 4.0).sin())
                    * (1.0 - 0.06 * (p[0] / 22.0).powi(2) + 0.05 * (p[2] / 16.0))
            }
        })
        .collect();
    Volume::from_vec(g, data).unwrap()
}

fn compare(g: &GpuN4, input: &Volume<f64>, params: &N4Params) -> f64 {
    let cpu = run_n4(input, params).unwrap();
    let started = std::time::Instant::now();
    let gpu = g.run_n4(input, params).unwrap();
    eprintln!(
        "gpu run_n4 wall time {:.3} s",
        started.elapsed().as_secs_f64()
    );

    let range = cpu
        .corrected
        .data()
        .iter()
        .fold(0.0f64, |a, b| a.max(f64::from(*b).abs()))
        .max(1e-9);
    let mut worst = 0.0f64;
    for (a, b) in gpu.corrected.data().iter().zip(cpu.corrected.data()) {
        worst = worst.max((f64::from(*a) - f64::from(*b)).abs());
    }
    let rel = worst / range;
    eprintln!("gpu vs cpu corrected: max abs {worst:.3e}, range {range:.3e}, rel {rel:.3e}");
    rel
}

#[test]
fn gpu_n4_matches_the_cpu_reference() {
    let Some(g) = gpu() else { return };
    let input = phantom([24, 24, 18], 2.0);
    let params = N4Params {
        shrink_factor: 2,
        iterations: vec![5; 4],
        ..N4Params::default()
    };
    let rel = compare(&g, &input, &params);
    assert!(rel <= 1e-4, "relative error {rel}");

    let cps: Vec<[usize; 3]> = g
        .run_n4(&input, &params)
        .unwrap()
        .levels
        .iter()
        .map(|l| l.control_points)
        .collect();
    assert_eq!(cps, vec![[4, 4, 4], [5, 5, 5], [7, 7, 7], [11, 11, 11]]);
    let _ = N4_SPLINE_ORDER;
}

#[test]
fn gpu_n4_matches_the_cpu_reference_at_the_production_schedule() {
    let Some(g) = gpu() else { return };
    let input = phantom([64, 64, 48], 1.0);
    let params = N4Params::default();
    assert_eq!(params.iterations, vec![50; 4]);
    let rel = compare(&g, &input, &params);
    assert!(rel <= 2e-4, "relative error {rel} at the 4x50 schedule");
}

#[test]
fn gpu_n4_is_deterministic() {
    let Some(g) = gpu() else { return };
    let input = phantom([20, 18, 16], 2.0);
    let params = N4Params {
        shrink_factor: 2,
        iterations: vec![4; 4],
        ..N4Params::default()
    };
    let a = g.run_n4(&input, &params).unwrap();
    let b = g.run_n4(&input, &params).unwrap();
    assert_eq!(a.corrected.data(), b.corrected.data());
    assert_eq!(a.bias_field.data(), b.bias_field.data());
}

#[test]
fn gpu_n4_handles_a_grid_past_the_1d_dispatch_limit() {
    let Some(g) = gpu() else { return };
    let size = [300usize, 256, 55];
    let n = (size[0] * size[1] * size[2]) as u64;
    let wg = 64u64;
    assert!(
        n.div_ceil(wg) > 65535,
        "the grid must exceed the 1-D workgroup limit, got {}",
        n.div_ceil(wg)
    );
    let input = phantom(size, 1.0);
    let params = N4Params {
        shrink_factor: 8,
        fitting_levels: 1,
        iterations: vec![1],
        ..N4Params::default()
    };
    let rel = compare(&g, &input, &params);
    assert!(rel <= 1e-4, "relative error {rel}");
}

#[test]
fn gpu_n4_stops_with_cancelled_when_the_flag_is_set() {
    let Some(g) = gpu() else { return };
    let input = phantom([24, 24, 20], 2.0);
    let params = N4Params {
        shrink_factor: 2,
        iterations: vec![2; 4],
        ..N4Params::default()
    };
    let set = std::sync::atomic::AtomicBool::new(true);
    assert!(matches!(
        g.run_n4_cancellable(&input, &params, Some(&set)),
        Err(GpuError::Cancelled)
    ));
    let clear = std::sync::atomic::AtomicBool::new(false);
    let a = g.run_n4_cancellable(&input, &params, Some(&clear)).unwrap();
    let b = g.run_n4(&input, &params).unwrap();
    assert_eq!(a.corrected.data(), b.corrected.data());
}
