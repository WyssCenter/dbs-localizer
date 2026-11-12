#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;

use fastwarp_reg::linalg::IDENTITY3;
use fastwarp_reg::linear::{run_rigid_registration_from, CpuLinearEngine, LinearRecipe};
use fastwarp_reg::preprocess::preprocess_volume_no_winsorize;
use fastwarp_reg::transform::itk_mat::write_mat_file;
use fastwarp_reg::{AffineTransform, Grid, TransformElem, Volume};

/// Metric value on the last `DIAGNOSTIC,<iteration>,<metric>,…` data line of ANTs stdout.
fn last_diagnostic_metric(stdout: &str) -> Option<f64> {
    let mut last = None;
    for line in stdout.lines() {
        let Some((_, rest)) = line.split_once("DIAGNOSTIC,") else {
            continue;
        };
        let mut fields = rest.split(',');
        let Some(iteration) = fields.next() else {
            continue;
        };
        let Some(metric) = fields.next() else {
            continue;
        };
        if iteration.trim().parse::<i64>().is_err() {
            continue;
        }
        if let Ok(v) = metric.trim().parse::<f64>() {
            last = Some(v);
        }
    }
    last
}

fn ants_registration_exe() -> Option<PathBuf> {
    let dir = fastwarp_core::env_var_os("FASTWARP_ANTS_DIR")?;
    let exe = if cfg!(windows) {
        "antsRegistration.exe"
    } else {
        "antsRegistration"
    };
    let p = Path::new(&dir).join(exe);
    assert!(
        p.is_file(),
        "FASTWARP_ANTS_DIR is set but {} is missing",
        p.display()
    );
    Some(p)
}

fn blob(g: Grid, centre: f64, shift: [f64; 3]) -> Volume<f64> {
    let data: Vec<f64> = (0..g.num_voxels())
        .map(|lin| {
            let [i, j, k] = g.ijk(lin);
            let p = g.index_to_point([i as f64, j as f64, k as f64]);
            let r2 = (p[0] - centre - shift[0]).powi(2)
                + (p[1] - centre - shift[1]).powi(2)
                + (p[2] - centre - shift[2]).powi(2);
            (-r2 / (2.0 * 18.0 * 18.0)).exp()
                + 0.15 * (0.3 * p[0] + 0.2 * p[1]).sin() * (0.25 * p[2]).cos()
        })
        .collect();
    Volume::from_vec(g, data).unwrap()
}

#[test]
#[ignore = "runs antsRegistration from FASTWARP_ANTS_DIR; slow, run with --ignored"]
fn the_final_metric_matches_the_ants_diagnostic_line() {
    let Some(exe) = ants_registration_exe() else {
        eprintln!("skipped: set FASTWARP_ANTS_DIR to the directory holding antsRegistration");
        return;
    };
    let dir = std::env::temp_dir().join(format!("fastwarp_coreg_metric_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let size = [64usize, 56, 48];
    let g = Grid::new(size, [2.0; 3], [-60.0, -52.0, -44.0], IDENTITY3).unwrap();
    let centre = 45.0;
    let fixed = blob(g, centre, [0.0; 3]);
    let moving = blob(g, centre, [4.0, -2.0, 1.5]);

    let fixed_path = dir.join("fixed.nii.gz");
    let moving_path = dir.join("moving.nii.gz");
    let to_f32 = |v: &Volume<f64>| {
        Volume::from_vec(*v.grid(), v.data().iter().map(|x| *x as f32).collect()).unwrap()
    };
    to_f32(&fixed).save_nifti(&fixed_path).unwrap();
    to_f32(&moving).save_nifti(&moving_path).unwrap();

    let start = AffineTransform::from_translation([1.0, 0.5, -0.5]);
    let start_path = dir.join("start.mat");
    write_mat_file(&start_path, &[TransformElem::Affine(start)]).unwrap();

    let prefix = dir.join("coarse_");
    let output = Command::new(&exe)
        .args(["--dimensionality", "3", "--output"])
        .arg(format!("[{}]", prefix.display()))
        .args(["--initial-moving-transform"])
        .arg(&start_path)
        .args(["--transform", "Rigid[0.1]", "--metric"])
        .arg(format!(
            "MI[{},{},1,32,Regular,0.25]",
            fixed_path.display(),
            moving_path.display()
        ))
        .args([
            "--convergence",
            "[200x100,1e-6,10]",
            "--shrink-factors",
            "8x4",
            "--smoothing-sigmas",
            "3x2vox",
            "--random-seed",
            "20260910",
            "--verbose",
            "1",
        ])
        .env("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "antsRegistration failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ants_metric = last_diagnostic_metric(&stdout).expect("no DIAGNOSTIC metric in stdout");

    let recipe = LinearRecipe {
        iterations: vec![200, 100],
        convergence_threshold: 1e-6,
        convergence_window: 10,
        shrink_factors: vec![8, 4],
        smoothing_sigmas: vec![3.0, 2.0],
        ..LinearRecipe::cohort_rigid()
    };
    let pre_fixed = preprocess_volume_no_winsorize(&fixed).unwrap();
    let pre_moving = preprocess_volume_no_winsorize(&moving).unwrap();
    let out =
        run_rigid_registration_from(&CpuLinearEngine, &recipe, &pre_fixed, &pre_moving, start)
            .unwrap();
    let cpu_metric = out.final_metric().expect("the last level took iterations");
    let rel = (cpu_metric - ants_metric).abs() / ants_metric.abs().max(1e-12);
    eprintln!("final metric: ANTs {ants_metric:.9}, CPU {cpu_metric:.9}, rel {rel:.3e}");
    assert!(
        rel < 1e-3,
        "final metric: ANTs {ants_metric} CPU {cpu_metric} rel {rel}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
