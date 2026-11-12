#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;

use fastwarp_reg::transform::ants_io::{
    with_suffix, write_composite_pair, COMPOSITE_SUFFIX, INVERSE_COMPOSITE_SUFFIX,
};
use fastwarp_reg::transform::itk_mat::write_mat_file;
use fastwarp_reg::{
    map_point_ras, AffineTransform, CompositePair, DisplacementField, Grid, TransformChain,
    TransformElem,
};
use ndarray::{Array3, Array4};
use nifti::writer::WriterOptions;
use nifti::{IntoNdArray, NiftiHeader, NiftiObject, ReaderOptions};

const TOL_MM: f64 = 1e-3;

fn ants_exe() -> Option<PathBuf> {
    let dir = fastwarp_core::env_var_os("FASTWARP_ANTS_DIR")?;
    let exe = if cfg!(windows) {
        "antsApplyTransforms.exe"
    } else {
        "antsApplyTransforms"
    };
    let p = Path::new(&dir).join(exe);
    assert!(
        p.is_file(),
        "FASTWARP_ANTS_DIR is set but {} is missing",
        p.display()
    );
    Some(p)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "fastwarp_reg_ants_compat_{}_{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ras_header(spacing: f64, lo: [f64; 3]) -> NiftiHeader {
    NiftiHeader {
        pixdim: [
            1.0,
            spacing as f32,
            spacing as f32,
            spacing as f32,
            1.0,
            1.0,
            1.0,
            1.0,
        ],
        sform_code: 2,
        qform_code: 0,
        srow_x: [spacing as f32, 0.0, 0.0, lo[0] as f32],
        srow_y: [0.0, spacing as f32, 0.0, lo[1] as f32],
        srow_z: [0.0, 0.0, spacing as f32, lo[2] as f32],
        xyzt_units: 0,
        ..NiftiHeader::default()
    }
}

struct Probe {
    coords: PathBuf,
    grid: PathBuf,
    grid_lo: [f64; 3],
    grid_n: [usize; 3],
}

fn make_probe(dir: &Path) -> Probe {
    let coord_sp = 8.0;
    let coord_lo = [-120.0; 3];
    let coord_n = 31usize;
    let coords = Array4::from_shape_fn((coord_n, coord_n, coord_n, 3), |(i, j, k, c)| {
        (coord_lo[c] + coord_sp * [i, j, k][c] as f64) as f32
    });
    let coords_path = dir.join("coords.nii.gz");
    WriterOptions::new(&coords_path)
        .reference_header(&ras_header(coord_sp, coord_lo))
        .write_nifti(&coords)
        .unwrap();
    let grid_lo = [-27.0, -26.0, -25.0];
    let grid_n = [55usize, 53, 51];
    let zeros = Array3::<u8>::zeros((grid_n[0], grid_n[1], grid_n[2]));
    let grid_path = dir.join("grid.nii.gz");
    WriterOptions::new(&grid_path)
        .reference_header(&ras_header(1.0, grid_lo))
        .write_nifti(&zeros)
        .unwrap();
    Probe {
        coords: coords_path,
        grid: grid_path,
        grid_lo,
        grid_n,
    }
}

fn run_ants(probe: &Probe, dir: &Path, tag: &str, tokens: &[String]) -> PathBuf {
    let out = dir.join(format!("out_{tag}.nii.gz"));
    let mut cmd = Command::new(ants_exe().unwrap());
    cmd.args(["-d", "3", "-e", "3", "-i"])
        .arg(&probe.coords)
        .arg("-r")
        .arg(&probe.grid)
        .arg("-o")
        .arg(&out)
        .args(["-n", "Linear"])
        .args(tokens)
        .env("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "2");
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "antsApplyTransforms failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    out
}

fn max_error(probe: &Probe, out: &Path, chain: &TransformChain) -> f64 {
    let obj = ReaderOptions::new().read_file(out).unwrap();
    let arr = obj.into_volume().into_ndarray::<f64>().unwrap();
    let shape = arr.shape().to_vec();
    assert_eq!(shape.len(), 4, "output shape {shape:?}");
    assert_eq!(shape[..3], probe.grid_n);
    assert_eq!(shape[3], 3);
    let mut worst = 0.0f64;
    let mut checked = 0usize;
    for k in (0..shape[2]).step_by(3) {
        for j in (0..shape[1]).step_by(3) {
            for i in (0..shape[0]).step_by(3) {
                let p = [
                    probe.grid_lo[0] + i as f64,
                    probe.grid_lo[1] + j as f64,
                    probe.grid_lo[2] + k as f64,
                ];
                let want = map_point_ras(chain, p);
                for c in 0..3 {
                    worst = worst.max((arr[[i, j, k, c]] - want[c]).abs());
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 1000);
    worst
}

fn pieces() -> (AffineTransform, DisplacementField, DisplacementField) {
    let (s, c) = 0.08f64.sin_cos();
    let dir = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
    let grid = Grid::new([11, 12, 10], [4.0, 3.5, 4.5], [-22.0, -20.0, -18.0], dir).unwrap();
    let affine = AffineTransform::new(
        [[1.02, 0.03, 0.0], [-0.03, 0.98, 0.01], [0.0, -0.01, 1.01]],
        [2.0, -1.5, 1.0],
        [1.0, 2.0, 3.0],
    );
    let warp = DisplacementField::from_fn(grid, |p| {
        [
            1.8 * (p[1] / 17.0).sin(),
            1.2 * (p[0] / 23.0).cos() - 0.5,
            0.9 * (p[2] / 13.0).sin(),
        ]
    });
    let inverse_warp = DisplacementField::from_fn(grid, |p| {
        [
            -1.7 * (p[1] / 19.0).sin(),
            -1.1 * (p[0] / 21.0).cos() + 0.4,
            -0.8 * (p[2] / 15.0).sin(),
        ]
    });
    (affine, warp, inverse_warp)
}

fn t(path: &Path) -> Vec<String> {
    vec!["-t".to_owned(), path.to_string_lossy().into_owned()]
}

fn t_inv(path: &Path) -> Vec<String> {
    vec!["-t".to_owned(), format!("[{},1]", path.to_string_lossy())]
}

#[test]
#[ignore = "runs antsApplyTransforms from FASTWARP_ANTS_DIR; slow, run with --ignored"]
fn ants_consumes_our_composite_pair_and_map_points_agrees() {
    let Some(_) = ants_exe() else {
        eprintln!("skipped: set FASTWARP_ANTS_DIR to the directory holding antsApplyTransforms");
        return;
    };
    let dir = tmp("pair");
    let (a, w, iw) = pieces();
    let pair = CompositePair::new(&a, w, iw).unwrap();
    let prefix = dir.join("reg_");
    write_composite_pair(&prefix, &pair).unwrap();
    let probe = make_probe(&dir);
    let fwd = run_ants(
        &probe,
        &dir,
        "fwd",
        &t(&with_suffix(&prefix, COMPOSITE_SUFFIX)),
    );
    let err = max_error(&probe, &fwd, &pair.forward);
    assert!(err < TOL_MM, "forward composite: max error {err} mm");
    let inv = run_ants(
        &probe,
        &dir,
        "inv",
        &t(&with_suffix(&prefix, INVERSE_COMPOSITE_SUFFIX)),
    );
    let err = max_error(&probe, &inv, &pair.inverse);
    assert!(err < TOL_MM, "inverse composite: max error {err} mm");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "runs antsApplyTransforms from FASTWARP_ANTS_DIR; slow, run with --ignored"]
fn ants_reads_a_collapsed_coregistration_affine_mat() {
    let Some(_) = ants_exe() else {
        eprintln!("skipped: set FASTWARP_ANTS_DIR to the directory holding antsApplyTransforms");
        return;
    };
    use fastwarp_reg::linalg::IDENTITY3;
    use fastwarp_reg::linear::{run_rigid_registration_from, CpuLinearEngine, LinearRecipe};
    use fastwarp_reg::Volume;
    let dir = tmp("collapse");
    let g = Grid::new([19, 19, 19], [2.0; 3], [0.0; 3], IDENTITY3).unwrap();
    let centre = 18.0;
    let blob = |shift: [f64; 3]| {
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let p = g.index_to_point([i as f64, j as f64, k as f64]);
                let r2 = (p[0] - centre - shift[0]).powi(2)
                    + (p[1] - centre - shift[1]).powi(2)
                    + (p[2] - centre - shift[2]).powi(2);
                (-r2 / (2.0 * 25.0 * 25.0)).exp()
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    };
    let fixed = blob([0.0; 3]);
    let moving = blob([4.0, -2.0, 1.0]);
    let recipe = LinearRecipe {
        iterations: vec![40, 30],
        shrink_factors: vec![2, 1],
        smoothing_sigmas: vec![1.0, 0.0],
        ..LinearRecipe::cohort_rigid()
    };
    let out = run_rigid_registration_from(
        &CpuLinearEngine,
        &recipe,
        &fixed,
        &moving,
        AffineTransform::from_translation([1.0, 0.5, -0.5]),
    )
    .unwrap();
    assert_eq!(out.total.center, out.rigid.affine.center);
    let mat = dir.join("collapsed.mat");
    write_mat_file(&mat, &[TransformElem::Affine(out.total)]).unwrap();
    let probe = make_probe(&dir);
    let chain = TransformChain::from_queue(vec![TransformElem::Affine(out.total)]);
    let image = run_ants(&probe, &dir, "collapse", &t(&mat));
    let err = max_error(&probe, &image, &chain);
    assert!(err < TOL_MM, "collapsed affine: max error {err} mm");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "runs antsApplyTransforms from FASTWARP_ANTS_DIR; slow, run with --ignored"]
fn ants_reads_our_mat_affine() {
    let Some(_) = ants_exe() else {
        eprintln!("skipped: set FASTWARP_ANTS_DIR to the directory holding antsApplyTransforms");
        return;
    };
    let dir = tmp("affine");
    let (a, _, _) = pieces();
    let probe = make_probe(&dir);
    let mat = dir.join("a.mat");
    write_mat_file(&mat, &[TransformElem::Affine(a)]).unwrap();
    let chain = TransformChain::from_queue(vec![TransformElem::Affine(a)]);
    let out = run_ants(&probe, &dir, "mat", &t(&mat));
    let err = max_error(&probe, &out, &chain);
    assert!(err < TOL_MM, "mat: max error {err} mm");
    let inverse = TransformChain::from_queue(vec![TransformElem::Affine(a.inverse().unwrap())]);
    let out = run_ants(&probe, &dir, "matinv", &t_inv(&mat));
    let err = max_error(&probe, &out, &inverse);
    assert!(err < TOL_MM, "[mat,1]: max error {err} mm");
    let _ = std::fs::remove_dir_all(&dir);
}
