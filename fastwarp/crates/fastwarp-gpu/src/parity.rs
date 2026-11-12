// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Operation-level parity of the wgpu backend against the `f64` CPU reference.
//!
//! The same checks run as the known-answer test at device creation (small case) and as the
//! level-1 tests on larger awkward grids. Every operation reports its largest absolute error, the
//! reference range it is judged against, and the tolerance of the plan's `f32` budget.

use fastwarp_reg::backend::Backend;
use fastwarp_reg::cpu::CpuBackend;
use fastwarp_reg::error::{RegError, Result as RegResult};
use fastwarp_reg::field_ops::{InvertParams, VectorField};
use fastwarp_reg::grid::Grid;
use fastwarp_reg::linalg::{Mat3, Vec3};
use fastwarp_reg::linear::{prepare_samples, CpuLinearEngine, LinearEngine};
use fastwarp_reg::optimizer::LinearMetric;
use fastwarp_reg::sampling::{metric_sample_points, SamplingSpec, SamplingStrategy, SeedSequence};
use fastwarp_reg::transform::{AffineTransform, LinearMovingTransform, ParametricTransform};
use fastwarp_reg::volume::Volume;

use crate::wgpu_backend::WgpuBackend;

/// Tolerances (largest absolute error over the reference range) of each operation.
pub mod tol {
    /// Upload then download of a field (`f32` rounding).
    pub const ROUND_TRIP: f64 = 2e-7;
    /// Image warp, away from buffer-edge validity flips.
    pub const WARP: f64 = 1e-5;
    /// CC metric value, relative.
    pub const CC_VALUE: f64 = 1e-5;
    /// CC derivative, voxels at least one voxel inside every face.
    pub const CC_DERIVATIVE: f64 = 1e-4;
    /// Gaussian smoothing.
    pub const SMOOTH: f64 = 2e-6;
    /// Norm reductions, relative.
    pub const NORM: f64 = 1e-6;
    /// Elementwise scale and axpy.
    pub const ELEMENTWISE: f64 = 1e-6;
    /// Field composition.
    pub const COMPOSE: f64 = 1e-5;
    /// Inversion: the inverse field, relative to its range.
    pub const INVERT: f64 = 1e-4;
    /// Field resampling onto another grid.
    pub const RESAMPLE: f64 = 1e-5;
    /// Mattes MI value, relative.
    pub const MI_VALUE: f64 = 1e-5;
    /// Mattes MI derivative, `L2` relative over the parameters.
    pub const MI_DERIVATIVE: f64 = 5e-4;
}

/// Geometry and content of one parity case.
#[derive(Clone, Debug)]
pub struct Case {
    /// Voxels per axis of the main grid.
    pub size: [usize; 3],
    /// Spacing of the main grid.
    pub spacing: [f64; 3],
    /// Origin of the main grid.
    pub origin: Vec3,
    /// Direction matrix of the main grid.
    pub direction: Mat3,
    /// CC window radius.
    pub radius: usize,
    /// Seed of the pseudo-random content.
    pub seed: u64,
}

fn rot_z(angle: f64) -> Mat3 {
    let (s, c) = angle.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

fn rot_x(angle: f64) -> Mat3 {
    let (s, c) = angle.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    fastwarp_reg::linalg::mat3_mul(a, b)
}

impl Case {
    /// The small case used at device creation: odd sizes, anisotropic spacing, oblique direction.
    pub fn selftest() -> Self {
        Case {
            size: [11, 9, 8],
            spacing: [1.5, 2.0, 1.25],
            origin: [-8.0, 5.0, -3.5],
            direction: mul(&rot_z(0.3), &rot_x(0.2)),
            radius: 2,
            seed: 7,
        }
    }

    /// A larger awkward case.
    pub fn awkward(size: [usize; 3], spacing: [f64; 3], oblique: bool, radius: usize) -> Self {
        Case {
            size,
            spacing,
            origin: [-12.5, 7.25, -4.0],
            direction: if oblique {
                mul(&rot_z(-0.41), &rot_x(0.27))
            } else {
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            },
            radius,
            seed: 11,
        }
    }

    /// The MI known-answer case used at device creation: 18×17×16, anisotropic and slightly
    /// oblique, so the histogram, the value and the derivative run off the cheap path.
    pub fn selftest_mi() -> Self {
        Case {
            size: [18, 17, 16],
            spacing: [1.3, 1.1, 1.5],
            origin: [-11.0, 7.0, -5.5],
            direction: mul(&rot_z(0.15), &rot_x(0.1)),
            radius: 2,
            seed: 23,
        }
    }

    /// The main grid.
    pub fn grid(&self) -> RegResult<Grid> {
        Grid::new(self.size, self.spacing, self.origin, self.direction)
    }

    /// A second grid with different size, spacing, origin and orientation, overlapping the main
    /// one.
    pub fn other_grid(&self) -> RegResult<Grid> {
        let size = [
            self.size[0] + 3,
            self.size[1].saturating_sub(1).max(2),
            self.size[2] + 2,
        ];
        let spacing = [
            self.spacing[0] * 0.8,
            self.spacing[1] * 1.15,
            self.spacing[2] * 0.9,
        ];
        let origin = [
            self.origin[0] + 1.3,
            self.origin[1] - 0.9,
            self.origin[2] + 0.7,
        ];
        Grid::new(size, spacing, origin, mul(&rot_z(0.17), &self.direction))
    }
}

/// Outcome of one operation check.
#[derive(Clone, Debug)]
pub struct OpResult {
    /// Operation name.
    pub op: String,
    /// Largest absolute error.
    pub max_abs: f64,
    /// Reference range the error is judged against.
    pub range: f64,
    /// `max_abs / range`.
    pub rel: f64,
    /// Tolerance on `rel`.
    pub tol: f64,
    /// Whether `rel <= tol` and no flag was raised.
    pub ok: bool,
    /// Extra facts (flips, iteration counts, border errors).
    pub note: String,
}

impl OpResult {
    /// One report line.
    pub fn line(&self) -> String {
        format!(
            "{}: max abs {:.3e}, range {:.3e}, rel {:.3e}, tol {:.1e}, {}{}",
            self.op,
            self.max_abs,
            self.range,
            self.rel,
            self.tol,
            if self.ok { "ok" } else { "FAIL" },
            if self.note.is_empty() {
                String::new()
            } else {
                format!(" ({})", self.note)
            }
        )
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn unit(seed: u64, i: usize) -> f64 {
    (splitmix(seed.wrapping_mul(0x1000_0000_01B3).wrapping_add(i as u64)) >> 11) as f64
        / (1u64 << 53) as f64
}

/// A smooth textured image in `[0.05, 0.95]` plus noise, deterministic in `seed`.
pub fn image(grid: &Grid, seed: u64, noise: f64) -> Volume<f64> {
    let data = (0..grid.num_voxels())
        .map(|idx| {
            let [i, j, k] = grid.ijk(idx);
            let (x, y, z) = (i as f64, j as f64, k as f64);
            let s = 0.5
                + 0.25 * (0.41 * x + 0.23 * y + seed as f64).sin() * (0.37 * z + 0.19 * y).cos()
                + 0.15 * (0.29 * x - 0.31 * z).sin();
            (s + noise * (unit(seed, idx) - 0.5)).clamp(0.02, 0.98)
        })
        .collect();
    Volume::from_vec(*grid, data).unwrap_or_else(|_| Volume::zeros(*grid))
}

/// A smooth displacement field with amplitude `amp` millimetres plus a little noise.
pub fn field(grid: &Grid, amp: f64, seed: u64) -> VectorField {
    let [nx, ny, nz] = grid.size();
    let data: Vec<Vec3> = (0..grid.num_voxels())
        .map(|idx| {
            let [i, j, k] = grid.ijk(idx);
            let u = i as f64 / nx as f64;
            let v = j as f64 / ny as f64;
            let w = k as f64 / nz as f64;
            let n = |c: usize| unit(seed + c as u64, idx) - 0.5;
            [
                amp * ((6.0 * v + 2.0 * w).sin() + 0.1 * n(0)),
                amp * (0.7 * (5.0 * w + 3.0 * u).cos() + 0.1 * n(1)),
                amp * (0.5 * (4.0 * u - 3.0 * v).sin() + 0.1 * n(2)),
            ]
        })
        .collect();
    VectorField::from_vec(*grid, data).unwrap_or_else(|_| VectorField::zeros(*grid))
}

fn flatten(f: &VectorField) -> Vec<f64> {
    f.data().iter().flat_map(|v| v.iter().copied()).collect()
}

fn compare(op: &str, got: &[f64], want: &[f64], tol: f64, note: String) -> OpResult {
    let range = want.iter().fold(0.0f64, |a, b| a.max(b.abs())).max(1e-12);
    let max_abs = got
        .iter()
        .zip(want)
        .fold(0.0f64, |a, (g, w)| a.max((g - w).abs()));
    let max_abs = if got.len() == want.len() && max_abs.is_finite() {
        max_abs
    } else {
        f64::INFINITY
    };
    let rel = max_abs / range;
    OpResult {
        op: op.to_string(),
        max_abs,
        range,
        rel,
        tol,
        ok: rel <= tol,
        note,
    }
}

fn is_interior(grid: &Grid, idx: usize, margin: usize) -> bool {
    let [i, j, k] = grid.ijk(idx);
    let [nx, ny, nz] = grid.size();
    i >= margin
        && j >= margin
        && k >= margin
        && i + margin < nx
        && j + margin < ny
        && k + margin < nz
}

fn mismatch(what: &str, e: RegError) -> RegError {
    RegError::Invalid {
        what: "parity",
        message: format!("{what}: {e}"),
    }
}

/// Runs every operation of the backend on `case` against the CPU reference.
pub fn check_all(gpu: &WgpuBackend, case: &Case) -> RegResult<Vec<OpResult>> {
    let cpu = CpuBackend;
    let grid = case.grid()?;
    let other = case.other_grid()?;
    let n = grid.num_voxels();
    let mut out = Vec::new();

    let amp = 1.4 * grid.spacing()[0];
    let f0 = field(&grid, amp, case.seed);
    let gf = gpu.upload_field(&f0)?;
    let cf = cpu.upload_field(&f0)?;

    let back = gpu.download_field(&gf)?;
    out.push(compare(
        "upload_field/download_field",
        &flatten(&back),
        &flatten(&f0),
        tol::ROUND_TRIP,
        String::new(),
    ));

    let src_img = image(&other, case.seed + 1, 0.1);
    let g_src = gpu.upload_image(&src_img)?;
    let c_src = cpu.upload_image(&src_img)?;
    let affine = AffineTransform::new(
        mul(&rot_z(0.07), &rot_x(-0.05)),
        [0.9, -0.6, 0.4],
        [1.0, 2.0, -1.0],
    );
    for (name, aff) in [
        ("warp_through_field", None),
        ("warp_through_field+affine", Some(&affine)),
    ] {
        let g = gpu
            .warp_through_field(&g_src, &gf, aff)
            .map_err(|e| mismatch(name, e))?;
        let c = cpu.warp_through_field(&c_src, &cf, aff)?;
        let got: Vec<f64> = gpu.download_image(&g)?;
        let want = c.data();
        let flips = got
            .iter()
            .zip(want)
            .filter(|(a, b)| (**a == 0.0) != (**b == 0.0))
            .count();
        let (gv, wv): (Vec<f64>, Vec<f64>) = got
            .iter()
            .zip(want)
            .filter(|(a, b)| (**a == 0.0) == (**b == 0.0))
            .map(|(a, b)| (*a, *b))
            .unzip();
        let mut r = compare(name, &gv, &wv, tol::WARP, format!("validity flips {flips}"));
        let outside = want.iter().filter(|v| **v == 0.0).count();
        r.note = format!("validity flips {flips}, outside {outside} of {n}");
        r.ok = r.ok && flips <= 1 + n / 500;
        out.push(r);
    }

    let fixed = image(&grid, case.seed + 2, 0.2);
    let moving = image(&grid, case.seed + 3, 0.2);
    let g_fix = gpu.upload_image(&fixed)?;
    let g_mov = gpu.upload_image(&moving)?;
    let c_fix = cpu.upload_image(&fixed)?;
    let c_mov = cpu.upload_image(&moving)?;
    let (gv, gd) = gpu.cc_value_derivative(&g_fix, &g_mov, case.radius)?;
    let (cv, cd) = cpu.cc_value_derivative(&c_fix, &c_mov, case.radius)?;
    let gd = gpu.download_field(&gd)?;
    let value_err = (gv - cv).abs() / cv.abs().max(1e-12);
    out.push(OpResult {
        op: "cc_value".to_string(),
        max_abs: (gv - cv).abs(),
        range: cv.abs(),
        rel: value_err,
        tol: tol::CC_VALUE,
        ok: value_err <= tol::CC_VALUE,
        note: format!("gpu {gv:.8}, cpu {cv:.8}"),
    });
    let (mut gi, mut ci, mut gb, mut cb) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (idx, (g, c)) in gd.data().iter().zip(cd.data()).enumerate() {
        let (tg, tc) = if is_interior(&grid, idx, 1) {
            (&mut gi, &mut ci)
        } else {
            (&mut gb, &mut cb)
        };
        tg.extend_from_slice(g);
        tc.extend_from_slice(c);
    }
    let range_all = cd
        .data()
        .iter()
        .flat_map(|v| v.iter())
        .fold(0.0f64, |a, b| a.max(b.abs()))
        .max(1e-12);
    let border = gb
        .iter()
        .zip(&cb)
        .fold(0.0f64, |a, (g, c)| a.max((g - c).abs()))
        / range_all;
    let mut r = compare(
        "cc_derivative (interior)",
        &gi,
        &ci,
        tol::CC_DERIVATIVE,
        format!("face voxels: max abs / range {border:.2e}"),
    );
    r.range = r.range.max(range_all);
    r.rel = r.max_abs / r.range;
    r.ok = r.rel <= tol::CC_DERIVATIVE;
    out.push(r);

    let ((pv_a, pd_a), (pv_b, pd_b)) = gpu.cc_value_derivative_pair(&g_fix, &g_mov, case.radius)?;
    let (sv_b, sd_b) = gpu.cc_value_derivative(&g_mov, &g_fix, case.radius)?;
    let (cv_b, cd_b) = cpu.cc_value_derivative(&c_mov, &c_fix, case.radius)?;
    let pd_a = flatten(&gpu.download_field(&pd_a)?);
    let pd_b = flatten(&gpu.download_field(&pd_b)?);
    let sd_b = flatten(&gpu.download_field(&sd_b)?);
    let swapped_range = cd_b
        .data()
        .iter()
        .flat_map(|v| v.iter())
        .fold(0.0f64, |a, b| a.max(b.abs()))
        .max(1e-12);
    let mut swapped_cpu = (Vec::new(), Vec::new());
    for (idx, (g, c)) in pd_b.chunks(3).zip(cd_b.data()).enumerate() {
        if is_interior(&grid, idx, 1) {
            swapped_cpu.0.extend_from_slice(g);
            swapped_cpu.1.extend_from_slice(c);
        }
    }
    let mut r = compare(
        "cc_derivative pair, swapped (interior)",
        &swapped_cpu.0,
        &swapped_cpu.1,
        tol::CC_DERIVATIVE,
        format!("pair value gpu {pv_b:.8}, cpu {cv_b:.8}"),
    );
    r.range = r.range.max(swapped_range);
    r.rel = r.max_abs / r.range;
    r.ok = r.ok && (pv_b - cv_b).abs() / cv_b.abs().max(1e-12) <= tol::CC_VALUE;
    out.push(r);
    let same = |a: &[f64], b: &[f64]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    };
    let first_direction = gpu.cc_value_derivative(&g_fix, &g_mov, case.radius)?;
    let first_direction = flatten(&gpu.download_field(&first_direction.1)?);
    let identical = pv_b.to_bits() == sv_b.to_bits()
        && pv_a.to_bits() == gv.to_bits()
        && same(&pd_b, &sd_b)
        && same(&pd_a, &first_direction);
    out.push(OpResult {
        op: "cc_derivative pair equals two calls".to_string(),
        max_abs: 0.0,
        range: 1.0,
        rel: 0.0,
        tol: 0.0,
        ok: identical,
        note: "bit-identical values and fields in both directions".to_string(),
    });

    for variance in [3.0, 0.3, 0.8, 14.0] {
        let g = gpu.smooth_field(gpu.copy_field(&gf)?, variance)?;
        let c = cpu.smooth_field(cpu.copy_field(&cf)?, variance)?;
        let tiled = gpu.smoothing_is_tiled(variance, grid.size())?;
        let g = flatten(&gpu.download_field(&g)?);
        out.push(compare(
            &format!("smooth_field variance {variance}"),
            &g,
            &flatten(&c),
            tol::SMOOTH,
            format!(
                "{} kernels",
                if tiled { "tiled" } else { "gather fallback" }
            ),
        ));
        let gather = gpu.smooth_field_gather(&gf, variance)?;
        let gather = flatten(&gpu.download_field(&gather)?);
        out.push(OpResult {
            op: format!("smooth_field variance {variance} equals gather kernel"),
            max_abs: 0.0,
            range: 1.0,
            rel: 0.0,
            tol: 0.0,
            ok: g.len() == gather.len()
                && g.iter()
                    .zip(&gather)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
            note: "bit-identical".to_string(),
        });
    }

    let gm = gpu.max_scaled_norm(&gf)?;
    let cm = cpu.max_scaled_norm(&cf)?;
    out.push(scalar("max_scaled_norm", gm, cm, tol::NORM));
    let gl = gpu.l2_norm(&gf)?;
    let cl = cpu.l2_norm(&cf)?;
    out.push(scalar("l2_norm", gl, cl, tol::NORM));

    let mut g_acc = gpu.copy_field(&gf)?;
    let mut c_acc = cpu.copy_field(&cf)?;
    gpu.scale(&mut g_acc, 0.37)?;
    cpu.scale(&mut c_acc, 0.37)?;
    out.push(compare(
        "scale",
        &flatten(&gpu.download_field(&g_acc)?),
        &flatten(&c_acc),
        tol::ELEMENTWISE,
        String::new(),
    ));
    let f1 = field(&grid, 0.5 * amp, case.seed + 5);
    let g1 = gpu.upload_field(&f1)?;
    let c1 = cpu.upload_field(&f1)?;
    gpu.axpy(&mut g_acc, -1.3, &g1)?;
    cpu.axpy(&mut c_acc, -1.3, &c1)?;
    out.push(compare(
        "axpy",
        &flatten(&gpu.download_field(&g_acc)?),
        &flatten(&c_acc),
        tol::ELEMENTWISE,
        String::new(),
    ));

    let big = field(&grid, 3.0 * grid.spacing()[0], case.seed + 9);
    let gb_ = gpu.upload_field(&big)?;
    let cb_ = cpu.upload_field(&big)?;
    let gc = gpu.compose(&gf, &gb_)?;
    let cc = cpu.compose(&cf, &cb_)?;
    out.push(compare(
        "compose",
        &flatten(&gpu.download_field(&gc)?),
        &flatten(&cc),
        tol::COMPOSE,
        String::new(),
    ));

    let small = field(&grid, 0.6 * grid.spacing()[0], case.seed + 13);
    let gs = gpu.upload_field(&small)?;
    let cs = cpu.upload_field(&small)?;
    let g_zero = gpu.zero_field(&grid)?;
    let c_zero = cpu.zero_field(&grid)?;
    let (gi_f, gst) = gpu.invert(&gs, gpu.copy_field(&g_zero)?, &InvertParams::SYN)?;
    let (ci_f, cst) = cpu.invert(&cs, cpu.copy_field(&c_zero)?, &InvertParams::SYN)?;
    let mut r = compare(
        "invert",
        &flatten(&gpu.download_field(&gi_f)?),
        &flatten(&ci_f),
        tol::INVERT,
        format!(
            "iterations gpu {} cpu {}, mean error gpu {:.2e} cpu {:.2e}",
            gst.iterations, cst.iterations, gst.mean_error_norm, cst.mean_error_norm
        ),
    );
    r.ok = r.ok && gst.iterations.abs_diff(cst.iterations) <= 1;
    out.push(r);

    let mut tiny = field(&grid, 0.08 * grid.spacing()[0], case.seed + 17);
    for idx in 0..n {
        if !is_interior(&grid, idx, 1) {
            tiny.data_mut()[idx] = [0.0; 3];
        }
    }
    let gt = gpu.upload_field(&tiny)?;
    let ct = cpu.upload_field(&tiny)?;
    let (gt_i, gt_s) = gpu.invert(&gt, g_zero, &InvertParams::SYN)?;
    let (ct_i, ct_s) = cpu.invert(&ct, c_zero, &InvertParams::SYN)?;
    let mut r = compare(
        "invert (early stop)",
        &flatten(&gpu.download_field(&gt_i)?),
        &flatten(&ct_i),
        tol::INVERT,
        format!(
            "iterations gpu {} cpu {}, max error gpu {:.2e} cpu {:.2e}",
            gt_s.iterations, ct_s.iterations, gt_s.max_error_norm, ct_s.max_error_norm
        ),
    );
    r.ok = r.ok && gt_s.iterations.abs_diff(ct_s.iterations) <= 1 && ct_s.iterations < 20;
    out.push(r);

    let g_re = gpu.resample_field(&gf, &other)?;
    let c_re = cpu.resample_field(&cf, &other)?;
    out.push(compare(
        "resample_field",
        &flatten(&gpu.download_field(&g_re)?),
        &flatten(&c_re),
        tol::RESAMPLE,
        String::new(),
    ));

    Ok(out)
}

/// Runs the Mattes mutual-information kernels of the backend on `case` against the CPU reference.
pub fn check_mi(gpu: &WgpuBackend, case: &Case) -> RegResult<Vec<OpResult>> {
    check_mi_with(gpu, case, 0.25, None, "")
}

/// The MI known-answer run with every grid voxel sampled and 256 samples per workgroup, so the
/// histogram kernel runs workgroups past the first and the merge kernel loops over many stripes.
pub fn check_mi_chunked(gpu: &WgpuBackend, case: &Case) -> RegResult<Vec<OpResult>> {
    check_mi_with(
        gpu,
        case,
        1.0,
        Some(MI_CHUNKED_SAMPLES_PER_GROUP),
        "chunked ",
    )
}

/// Samples per workgroup of [`check_mi_chunked`].
pub const MI_CHUNKED_SAMPLES_PER_GROUP: u32 = 256;

fn check_mi_with(
    gpu: &WgpuBackend,
    case: &Case,
    percentage: f32,
    chunk: Option<u32>,
    label: &str,
) -> RegResult<Vec<OpResult>> {
    let grid = case.grid()?;
    let fixed = image(&grid, case.seed + 31, 0.15);
    let moving = image(&grid, case.seed + 32, 0.15);
    let raw = metric_sample_points(
        &grid,
        SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage,
        },
        &mut SeedSequence::new(20_260_910),
        None,
    );
    let samples = prepare_samples(&fixed, &moving, &raw, 32)?;
    let cpu = CpuLinearEngine.prepare_level(fixed.clone(), moving.clone(), samples.clone())?;
    let gpu_level = match chunk {
        Some(c) => gpu.prepare_level_with_chunk(moving, samples, c)?,
        None => gpu.prepare_level(fixed, moving, samples)?,
    };

    let s = grid.size();
    let centre = grid.index_to_point([
        (s[0] as f64 - 1.0) / 2.0,
        (s[1] as f64 - 1.0) / 2.0,
        (s[2] as f64 - 1.0) / 2.0,
    ]);
    let rigid = LinearMovingTransform {
        outer: AffineTransform::from_translation([0.4, -0.3, 0.2]),
        optimised: ParametricTransform::euler3d([0.02, -0.03, 0.04, 0.5, -0.4, 0.3], centre),
    };
    let affine = LinearMovingTransform {
        outer: AffineTransform::from_translation([0.2, 0.1, -0.2]),
        optimised: ParametricTransform::affine(&AffineTransform::new(
            [[1.01, 0.01, 0.0], [-0.01, 0.99, 0.01], [0.0, 0.0, 1.02]],
            [0.0, 0.0, 0.0],
            centre,
        )),
    };

    let mut out = Vec::new();
    for (name, t) in [("rigid", &rigid), ("affine", &affine)] {
        let name = format!("mi {label}({name})");
        let name = name.as_str();
        let (cv, cd) = cpu.value_and_derivative(t)?;
        let (gv, gd) = gpu_level.value_and_derivative(t)?;
        let vrel = (gv - cv).abs() / cv.abs().max(1e-12);
        out.push(OpResult {
            op: format!("{name} value"),
            max_abs: (gv - cv).abs(),
            range: cv.abs().max(1e-12),
            rel: vrel,
            tol: tol::MI_VALUE,
            ok: vrel <= tol::MI_VALUE,
            note: format!("gpu {gv:.8}, cpu {cv:.8}"),
        });
        let (mut num, mut den) = (0.0, 0.0);
        for (a, b) in cd.iter().zip(&gd) {
            num += (a - b) * (a - b);
            den += a * a;
        }
        let drel = (num / den.max(1e-30)).sqrt();
        out.push(OpResult {
            op: format!("{name} derivative"),
            max_abs: num.sqrt(),
            range: den.sqrt().max(1e-12),
            rel: drel,
            tol: tol::MI_DERIVATIVE,
            ok: drel <= tol::MI_DERIVATIVE,
            note: format!("{} parameters", cd.len()),
        });
    }
    Ok(out)
}

fn scalar(op: &str, got: f64, want: f64, tol: f64) -> OpResult {
    let max_abs = (got - want).abs();
    let range = want.abs().max(1e-12);
    let rel = max_abs / range;
    OpResult {
        op: op.to_string(),
        max_abs,
        range,
        rel,
        tol,
        ok: rel <= tol,
        note: format!("gpu {got:.8}, cpu {want:.8}"),
    }
}
