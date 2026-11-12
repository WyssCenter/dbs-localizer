#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

use fastwarp_reg::linalg::IDENTITY3;
use fastwarp_reg::linear::{run_linear_stage, CpuLinearEngine, LinearRecipe};
use fastwarp_reg::{AffineTransform, Grid, Volume};

/// Peak working set of the current process in bytes, read from the OS.
fn peak_working_set() -> Option<u64> {
    let script = format!("(Get-Process -Id {}).PeakWorkingSet64", std::process::id());
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[test]
#[ignore = "measures the host peak of the CT coreg smoothing path; allocates several GB"]
fn the_ct_coreg_smoothing_path_stays_under_the_host_budget() {
    if !cfg!(windows) {
        eprintln!("skip: the peak-working-set probe is Windows-only");
        return;
    }
    let size = [512usize, 512, 300];
    let g = Grid::new(size, [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
    let make = |phase: f64| {
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let (x, y, z) = (i as f64, j as f64, k as f64);
                0.5 + 0.25 * (0.01 * x + phase).sin() * (0.013 * y).cos() + 0.1 * (0.007 * z).sin()
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    };
    let fixed = make(0.0);
    let moving = make(0.7);
    let baseline = peak_working_set();
    let recipe = LinearRecipe {
        iterations: vec![1],
        shrink_factors: vec![4],
        smoothing_sigmas: vec![2.0],
        cache_smoothed_images: false,
        ..LinearRecipe::cohort_rigid()
    };
    let _ = run_linear_stage(
        &CpuLinearEngine,
        &recipe,
        &fixed,
        &moving,
        AffineTransform::identity(),
        fastwarp_reg::transform::ParametricTransform::euler3d([0.0; 6], [0.0; 3]),
    )
    .unwrap();
    let peak = peak_working_set();
    let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    eprintln!(
        "baseline peak {:?} GiB, run peak {:?} GiB",
        baseline.map(gib),
        peak.map(gib)
    );
    if let Some(p) = peak {
        assert!(gib(p) < 4.0, "peak {:.2} GiB above 4 GiB", gib(p));
    }
}
