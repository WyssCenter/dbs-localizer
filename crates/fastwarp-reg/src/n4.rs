// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
// SPDX-FileCopyrightText: ANTs developers
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0
//
// Copyright NumFOCUS (Insight Software Consortium)
// Copyright 2026 Wyss Center for Bio and Neuroengineering
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Portions derived from ITK `itkN4BiasFieldCorrectionImageFilter.hxx` (`GenerateData`,
// `SharpenImage`, `UpdateBiasFieldEstimate`, `ReconstructBiasField`,
// `CalculateConvergenceMeasurement`), `itkBSplineScatteredDataPointSetToImageFilter.hxx`
// (`GenerateData`, `ThreadedGenerateDataForFitting`, `AfterThreadedGenerateData`,
// `RefineControlPointLattice`, `CollapsePhiLattice`), `itkBSplineControlPointImageFilter.hxx`
// (`DynamicThreadedGenerateData`, `RefineControlPointLattice`), `itkBSplineKernelFunction.h`
// (order 3) and ANTs `N4BiasFieldCorrection.cxx` (the CLI defaults, the all-ones mask, the
// shrink factor and the full-resolution reconstruction). See `docs/THIRD_PARTY_PORTS.md`.

//! N4 bias-field correction in `f64` on the CPU: the parity oracle and the fallback of the
//! in-process preprocessing step.
//!
//! The algorithm reproduces the ANTs CLI defaults
//! (`N4BiasFieldCorrection -d 3 -i <raw anchor> -o <stem>_N4.nii.gz`): all-ones mask, shrink
//! factor 4, four fitting levels of 50 iterations with convergence threshold 0, cubic B-splines
//! from one initial mesh element (`[1x1x1]`, 4 control points per axis), histogram sharpening
//! `[0.15, 0.01, 200]`, and the bias field evaluated at full resolution as `input / exp(log bias)`.
//!
//! The scattered-data fit is the Lee-Wolberg-Shin multilevel approximation. ITK scatters every
//! voxel into the 64 control points around it; here every control point *gathers* the voxels of
//! a fixed index box in a fixed order, so the sum has one deterministic order (the GPU kernels
//! use the same one). All control-point geometry is index-space: with `p_d = i_d * spans_d /
//! (size_d - 1)` the fit is independent of the grid's physical spacing and origin, and the final
//! full-resolution reconstruction only changes `size_d`.
//!
//! The bias field is the sum of a control-point lattice refined (`2*CP - order` per level) and
//! re-fitted each iteration. The lattice is kept as `f64` on the host, matching the GPU host
//! loop; only the two per-point moments cross the device boundary.

use rayon::prelude::*;

use crate::error::{invalid, Result};
use crate::pyramid::shrink_volume;
use crate::volume::Volume;

/// Spline order the N4 control-point lattice uses (cubic).
pub const N4_SPLINE_ORDER: usize = 3;

/// Parameters of [`run_n4`]; the defaults are the ANTs CLI defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct N4Params {
    /// Shrink factor applied to the input before the fit (ANTs `-s`, default 4).
    pub shrink_factor: u32,
    /// Number of fitting levels (ANTs default 4).
    pub fitting_levels: usize,
    /// Iterations per level; the threshold is 0 so every iteration runs.
    pub iterations: Vec<usize>,
    /// B-spline order (must be 3).
    pub spline_order: usize,
    /// Control points per axis at the first level (ANTs `-b [1x1x1,3]` -> 4).
    pub number_of_control_points: [usize; 3],
    /// Histogram bins for the N3 sharpening step (ANTs default 200).
    pub number_of_histogram_bins: usize,
    /// Wiener filter noise (ANTs `-t` second value, default 0.01).
    pub wiener_filter_noise: f64,
    /// Gaussian deconvolution FWHM (ANTs `-t` first value, default 0.15).
    pub bias_field_fwhm: f64,
    /// Convergence threshold (ANTs `-c` second value, default 0).
    pub convergence_threshold: f64,
}

impl Default for N4Params {
    fn default() -> Self {
        Self {
            shrink_factor: 4,
            fitting_levels: 4,
            iterations: vec![50; 4],
            spline_order: N4_SPLINE_ORDER,
            number_of_control_points: [4, 4, 4],
            number_of_histogram_bins: 200,
            wiener_filter_noise: 0.01,
            bias_field_fwhm: 0.15,
            convergence_threshold: 0.0,
        }
    }
}

impl N4Params {
    /// Validates the parameters.
    pub fn validate(&self) -> Result<()> {
        if self.spline_order != N4_SPLINE_ORDER {
            return Err(invalid(
                "n4",
                format!("spline order {} is not implemented", self.spline_order),
            ));
        }
        if self.fitting_levels == 0 || self.iterations.len() < self.fitting_levels {
            return Err(invalid(
                "n4",
                "the iteration schedule must cover every fitting level",
            ));
        }
        for d in 0..3 {
            if self.number_of_control_points[d] < self.spline_order + 1 {
                return Err(invalid(
                    "n4",
                    "the number of control points must exceed the spline order",
                ));
            }
        }
        if self.number_of_histogram_bins < 2 {
            return Err(invalid("n4", "at least two histogram bins are required"));
        }
        Ok(())
    }
}

/// A scalar control-point lattice, x-fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlLattice {
    /// Control points per axis.
    pub size: [usize; 3],
    /// Values, x-fastest.
    pub data: Vec<f64>,
}

impl ControlLattice {
    /// A zero lattice of `size`.
    pub fn zeros(size: [usize; 3]) -> Self {
        Self {
            size,
            data: vec![0.0; size[0] * size[1] * size[2]],
        }
    }

    /// Wraps `data`, checking its length.
    pub fn from_vec(size: [usize; 3], data: Vec<f64>) -> Result<Self> {
        let n = size[0] * size[1] * size[2];
        if data.len() != n {
            return Err(invalid(
                "n4",
                format!("lattice has {} values for size {size:?}", data.len()),
            ));
        }
        Ok(Self { size, data })
    }

    /// Linear offset of control point `(i, j, k)`, x-fastest.
    pub fn linear(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.size[0] * (j + self.size[1] * k)
    }

    /// `self + other` pointwise (same size).
    pub fn add(&self, other: &Self) -> Result<Self> {
        if self.size != other.size {
            return Err(invalid("n4", "lattice sizes differ in add"));
        }
        let data = self
            .data
            .iter()
            .zip(&other.data)
            .map(|(a, b)| a + b)
            .collect();
        Ok(Self {
            size: self.size,
            data,
        })
    }
}

/// One fitting level of a run.
#[derive(Clone, Debug, PartialEq)]
pub struct N4LevelTrace {
    /// Zero-based level.
    pub level: usize,
    /// Iterations executed.
    pub iterations: usize,
    /// Control points per axis used during the level.
    pub control_points: [usize; 3],
    /// Voxels of the shrunk grid the level fits on.
    pub grid_size: [usize; 3],
    /// Convergence value after every iteration.
    pub convergence: Vec<f64>,
}

/// Output of [`run_n4`].
#[derive(Clone, Debug)]
pub struct N4Outcome {
    /// The corrected image (`input / exp(log bias)`) at full resolution, `f32` like ANTs writes.
    pub corrected: Volume<f32>,
    /// The multiplicative bias field `exp(log bias)` at full resolution, `f32`.
    pub bias_field: Volume<f32>,
    /// The final control-point lattice.
    pub lattice: ControlLattice,
    /// Per-level trace.
    pub levels: Vec<N4LevelTrace>,
}

/// The cubic B-spline kernel `BSplineKernelFunction<3>::Evaluate`.
pub fn cubic_bspline(u: f64) -> f64 {
    let a = u.abs();
    if a < 1.0 {
        let a2 = a * a;
        (4.0 - 6.0 * a2 + 3.0 * a2 * a) / 6.0
    } else if a < 2.0 {
        let a2 = a * a;
        (8.0 - 12.0 * a + 6.0 * a2 - a2 * a) / 6.0
    } else {
        0.0
    }
}

fn binom(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    let mut r = 1.0;
    for i in 0..k {
        r = r * (n - i) as f64 / (i + 1) as f64;
    }
    r
}

/// The 1-D refinement masks of [`ControlLattice`] subdivision for spline `order`:
/// `(row off = 0, row off = 1)`, each with `order + 1` entries.
///
/// Derived from the causal B-spline two-scale relation `B_n(x) = sum_k 2^{1-n} C(n,k)
/// B_n(2x - k)` (`n = order + 1`): fine control point `2*i + off` is
/// `sum_j a[order + off - 2*j] * coarse[i + j]`.
pub fn refine_coefficients(order: usize) -> (Vec<f64>, Vec<f64>) {
    let n = order + 1;
    let a: Vec<f64> = (0..=n)
        .map(|k| binom(n, k) * 2f64.powi(1 - n as i32))
        .collect();
    let mut rows = [vec![0.0; order + 1], vec![0.0; order + 1]];
    for (off, row) in rows.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            let idx = order as isize + off as isize - 2 * j as isize;
            if idx >= 0 && idx as usize <= n {
                *cell = a[idx as usize];
            }
        }
    }
    let mut it = rows.into_iter();
    let a0 = it.next().unwrap_or_default();
    let a1 = it.next().unwrap_or_default();
    (a0, a1)
}

/// Doubles the resolution of a lattice by `2*CP - order` per axis, preserving the spline
/// (`itkBSplineControlPointImageFilter::RefineControlPointLattice`).
pub fn refine_lattice(lattice: &ControlLattice, order: usize) -> Result<ControlLattice> {
    let (off0, off1) = refine_coefficients(order);
    let coeff = [off0, off1];
    let old = lattice.size;
    let new_size = [2 * old[0] - order, 2 * old[1] - order, 2 * old[2] - order];
    let [n0, n1, n2] = new_size;
    let mut out = vec![0.0; n0 * n1 * n2];
    let mut bx = 0;
    while bx < n0 {
        let mut by = 0;
        while by < n1 {
            let mut bz = 0;
            while bz < n2 {
                for off in 0..8usize {
                    let o = [off & 1, (off >> 1) & 1, (off >> 2) & 1];
                    let tmp = [bx + o[0], by + o[1], bz + o[2]];
                    if tmp[0] >= n0 || tmp[1] >= n1 || tmp[2] >= n2 {
                        continue;
                    }
                    let base = [bx / 2, by / 2, bz / 2];
                    let mut sum = 0.0;
                    for p in 0..64usize {
                        let q = [p & 3, (p >> 2) & 3, (p >> 4) & 3];
                        let tp = [base[0] + q[0], base[1] + q[1], base[2] + q[2]];
                        if tp[0] >= old[0] || tp[1] >= old[1] || tp[2] >= old[2] {
                            continue;
                        }
                        let c = coeff[o[0]][q[0]] * coeff[o[1]][q[1]] * coeff[o[2]][q[2]];
                        sum += c * lattice.data[lattice.linear(tp[0], tp[1], tp[2])];
                    }
                    out[tmp[0] + n0 * (tmp[1] + n1 * tmp[2])] = sum;
                }
                bz += 2;
            }
            by += 2;
        }
        bx += 2;
    }
    ControlLattice::from_vec(new_size, out)
}

fn parametric_clamp(p: &mut f64, spans: f64, epsilon: f64) -> bool {
    if (*p - spans).abs() <= epsilon {
        *p = spans - epsilon;
    }
    if *p < 0.0 && p.abs() <= epsilon {
        *p = 0.0;
    }
    *p >= 0.0 && *p < spans
}

fn shift(order: usize) -> f64 {
    0.5 * (order as f64 - 1.0)
}

/// Evaluates a lattice on an `out_size` grid (`itkBSplineControlPointImageFilter`, tensor-product
/// cubic). The value at voxel `t` is `sum_{i,j,k in 0..=order} prod_d cubic(frac_d - i_d + 1) *
/// lattice[floor(U_d) + i_d]` with `U_d = (CP_d - order) * t_d / (out_size_d - 1)`.
pub fn eval_lattice(
    lattice: &ControlLattice,
    out_size: [usize; 3],
    order: usize,
) -> Result<Vec<f64>> {
    for (d, &os) in out_size.iter().enumerate() {
        if os < 2 {
            return Err(invalid(
                "n4",
                "the evaluation grid needs at least two voxels",
            ));
        }
        if lattice.size[d] <= order {
            return Err(invalid(
                "n4",
                "the lattice is smaller than the spline order",
            ));
        }
    }
    let n = out_size[0] * out_size[1] * out_size[2];
    let spans = [
        (lattice.size[0] - order) as f64,
        (lattice.size[1] - order) as f64,
        (lattice.size[2] - order) as f64,
    ];
    let inv = [
        spans[0] / (out_size[0] - 1) as f64,
        spans[1] / (out_size[1] - 1) as f64,
        spans[2] / (out_size[2] - 1) as f64,
    ];
    let eps = [inv[0] * 1e-3, inv[1] * 1e-3, inv[2] * 1e-3];
    let sh = shift(order);
    let [nx, ny, _nz] = out_size;
    let out: Vec<f64> = (0..n)
        .into_par_iter()
        .map(|lin| {
            let i = lin % nx;
            let j = (lin / nx) % ny;
            let k = lin / (nx * ny);
            let idxd = [i, j, k];
            let mut u = [0.0f64; 3];
            for d in 0..3 {
                u[d] = inv[d] * idxd[d] as f64;
                if !parametric_clamp(&mut u[d], spans[d], eps[d]) {
                    u[d] = 0.0;
                }
            }
            let base = [
                u[0].floor() as usize,
                u[1].floor() as usize,
                u[2].floor() as usize,
            ];
            let frac = [
                u[0] - base[0] as f64,
                u[1] - base[1] as f64,
                u[2] - base[2] as f64,
            ];
            let mut val = 0.0;
            for o2 in 0..=order {
                let kz = base[2] + o2;
                if kz >= lattice.size[2] {
                    continue;
                }
                let wz = cubic_bspline(frac[2] - o2 as f64 + sh);
                for o1 in 0..=order {
                    let ky = base[1] + o1;
                    if ky >= lattice.size[1] {
                        continue;
                    }
                    let wyz = wz * cubic_bspline(frac[1] - o1 as f64 + sh);
                    for o0 in 0..=order {
                        let kx = base[0] + o0;
                        if kx >= lattice.size[0] {
                            continue;
                        }
                        let wx = cubic_bspline(frac[0] - o0 as f64 + sh);
                        val += wx * wyz * lattice.data[lattice.linear(kx, ky, kz)];
                    }
                }
            }
            val
        })
        .collect();
    Ok(out)
}

/// A precomputed scattered-data gather for one lattice resolution.
///
/// `w2sum` is the per-voxel `sum of weights^2` over its 64 control points; `bounds` is the
/// inclusive `(lo, hi)` voxel box of every control point, laid out x-fastest over the lattice.
/// Both are independent of the residual values, so the GPU uploads them once per level.
#[derive(Clone, Debug)]
pub struct FitPlan {
    /// Number of voxels of the (shrunk) grid, x-fastest.
    pub grid_size: [usize; 3],
    /// Control points per axis.
    pub lattice_size: [usize; 3],
    /// Spline order.
    pub order: usize,
    /// `spans_d / (grid_size_d - 1)`.
    pub scale: [f64; 3],
    /// The parametric coordinate `p_d = i_d * scale_d` per axis.
    pub p_axis: [Vec<f64>; 3],
    /// Per-voxel `sum of weights^2`, x-fastest.
    pub w2sum: Vec<f64>,
    /// Per-control-point inclusive voxel box `[lo_x, hi_x, lo_y, hi_y, lo_z, hi_z]`, x-fastest.
    pub bounds: Vec<[u32; 6]>,
}

impl FitPlan {
    /// Builds the gather plan.
    pub fn new(grid_size: [usize; 3], lattice_size: [usize; 3], order: usize) -> Result<Self> {
        for d in 0..3 {
            if grid_size[d] < 2 {
                return Err(invalid("n4", "the fitting grid needs at least two voxels"));
            }
            if lattice_size[d] < order + 1 {
                return Err(invalid("n4", "the lattice must exceed the spline order"));
            }
        }
        let spans = [
            (lattice_size[0] - order) as f64,
            (lattice_size[1] - order) as f64,
            (lattice_size[2] - order) as f64,
        ];
        let scale = [
            spans[0] / (grid_size[0] - 1) as f64,
            spans[1] / (grid_size[1] - 1) as f64,
            spans[2] / (grid_size[2] - 1) as f64,
        ];
        let eps = [scale[0] * 1e-3, scale[1] * 1e-3, scale[2] * 1e-3];
        let mut p_axis: [Vec<f64>; 3] = [
            Vec::with_capacity(grid_size[0]),
            Vec::with_capacity(grid_size[1]),
            Vec::with_capacity(grid_size[2]),
        ];
        for d in 0..3 {
            for i in 0..grid_size[d] {
                let mut p = i as f64 * scale[d];
                parametric_clamp(&mut p, spans[d], eps[d]);
                p_axis[d].push(p);
            }
        }
        let w2sum = voxel_w2sum(grid_size, &p_axis, order);
        let bounds = control_bounds(grid_size, lattice_size, order, &p_axis)?;
        Ok(Self {
            grid_size,
            lattice_size,
            order,
            scale,
            p_axis,
            w2sum,
            bounds,
        })
    }

    /// Gathers the residual into a control-point lattice (`ThreadedGenerateDataForFitting` plus
    /// `AfterThreadedGenerateData`, with unit point weights and a fixed box order).
    pub fn phi(&self, residual: &[f64]) -> Result<ControlLattice> {
        let n = self.grid_size[0] * self.grid_size[1] * self.grid_size[2];
        if residual.len() != n {
            return Err(invalid(
                "n4",
                format!("residual has {} values, expected {n}", residual.len()),
            ));
        }
        let [nx, ny, _] = self.grid_size;
        let [n0, n1, _] = self.lattice_size;
        let sh = shift(self.order);
        let data: Vec<f64> = (0..self.bounds.len())
            .into_par_iter()
            .map(|c| {
                let i = c % n0;
                let j = (c / n0) % n1;
                let k = c / (n0 * n1);
                let b = self.bounds[c];
                let mut num = 0.0;
                let mut den = 0.0;
                for z in b[4]..=b[5] {
                    let wz = cubic_bspline(self.p_axis[2][z as usize] - k as f64 + sh);
                    for y in b[2]..=b[3] {
                        let wyz = wz * cubic_bspline(self.p_axis[1][y as usize] - j as f64 + sh);
                        for x in b[0]..=b[1] {
                            let v = x as usize + nx * (y as usize + ny * z as usize);
                            let w2 = self.w2sum[v];
                            if w2 <= 0.0 {
                                continue;
                            }
                            let w = wyz * cubic_bspline(self.p_axis[0][x as usize] - i as f64 + sh);
                            num += residual[v] * w * w * w / w2;
                            den += w * w;
                        }
                    }
                }
                if den > 0.0 {
                    num / den
                } else {
                    0.0
                }
            })
            .collect();
        ControlLattice::from_vec(self.lattice_size, data)
    }
}

fn voxel_w2sum(grid_size: [usize; 3], p_axis: &[Vec<f64>; 3], order: usize) -> Vec<f64> {
    let n = grid_size[0] * grid_size[1] * grid_size[2];
    let sh = shift(order);
    (0..n)
        .into_par_iter()
        .map(|lin| {
            let i = lin % grid_size[0];
            let j = (lin / grid_size[0]) % grid_size[1];
            let k = lin / (grid_size[0] * grid_size[1]);
            let mut sum = 0.0;
            let fx = p_axis[0][i] - p_axis[0][i].floor();
            let fy = p_axis[1][j] - p_axis[1][j].floor();
            let fz = p_axis[2][k] - p_axis[2][k].floor();
            for o2 in 0..=order {
                let wz = cubic_bspline(fz - o2 as f64 + sh);
                for o1 in 0..=order {
                    let wyz = wz * cubic_bspline(fy - o1 as f64 + sh);
                    for o0 in 0..=order {
                        let w = wyz * cubic_bspline(fx - o0 as f64 + sh);
                        sum += w * w;
                    }
                }
            }
            sum
        })
        .collect()
}

fn control_bounds(
    grid_size: [usize; 3],
    lattice_size: [usize; 3],
    order: usize,
    p_axis: &[Vec<f64>; 3],
) -> Result<Vec<[u32; 6]>> {
    let base: [Vec<usize>; 3] = [
        p_axis[0].iter().map(|p| p.floor() as usize).collect(),
        p_axis[1].iter().map(|p| p.floor() as usize).collect(),
        p_axis[2].iter().map(|p| p.floor() as usize).collect(),
    ];
    let range = |axis: usize, c: usize| -> (usize, usize) {
        let axis_base = &base[axis];
        let len = grid_size[axis];
        let lo_target = c.saturating_sub(order);
        let lo = axis_base.partition_point(|b| *b < lo_target);
        let hi = axis_base.partition_point(|b| *b <= c);
        if lo >= len || hi <= lo {
            (0, 0)
        } else {
            (lo, (hi - 1).min(len - 1))
        }
    };
    let [n0, n1, n2] = lattice_size;
    let mut bounds = Vec::with_capacity(n0 * n1 * n2);
    for k in 0..n2 {
        for j in 0..n1 {
            for i in 0..n0 {
                let (x0, x1) = range(0, i);
                let (y0, y1) = range(1, j);
                let (z0, z1) = range(2, k);
                bounds.push([
                    x0 as u32, x1 as u32, y0 as u32, y1 as u32, z0 as u32, z1 as u32,
                ]);
            }
        }
    }
    Ok(bounds)
}

/// The histogram table `E` of the N3 sharpening step plus the mapping needed to index it.
#[derive(Clone, Debug, PartialEq)]
pub struct SharpenLut {
    /// Lowest log value of the histogram.
    pub bin_min: f64,
    /// Highest log value of the histogram.
    pub bin_max: f64,
    /// `(bin_max - bin_min) / (bins - 1)`.
    pub slope: f64,
    /// The expectation table, one value per histogram bin.
    pub table: Vec<f64>,
}

/// The sequential min/max rule of `SharpenImage` (`binMaximum` first, `binMinimum` on the first
/// non-maximum), which is what makes ANTs' histogram binning reproducible.
pub fn histogram_range(values: &[f64]) -> (f64, f64) {
    let mut bin_max = f64::MIN;
    let mut bin_min = f64::MAX;
    for v in values {
        if *v > bin_max {
            bin_max = *v;
        } else if *v < bin_min {
            bin_min = *v;
        }
    }
    (bin_min, bin_max)
}

/// Builds the sharpening table of `SharpenImage` from the uncorrected log image.
pub fn sharpen_lut(
    values: &[f64],
    bins: usize,
    fwhm: f64,
    wiener_noise: f64,
) -> Result<SharpenLut> {
    if values.len() < 2 {
        return Err(invalid("n4", "sharpen needs at least two voxels"));
    }
    let (bin_min, bin_max) = histogram_range(values);
    let (bin_min, bin_max) = if bin_max > bin_min {
        (bin_min, bin_max)
    } else {
        let (lo, hi) = values
            .iter()
            .copied()
            .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        (lo, hi)
    };
    let slope = (bin_max - bin_min) / (bins - 1) as f64;
    if !(slope.is_finite() && slope > 0.0) {
        let lo = bin_min.min(bin_max);
        let table: Vec<f64> = (0..bins).map(|i| lo + i as f64).collect();
        return Ok(SharpenLut {
            bin_min: lo,
            bin_max: lo + (bins - 1) as f64,
            slope: 1.0,
            table,
        });
    }
    let padded = padded_size(bins);
    let offset = (padded - bins) / 2;
    let mut hist = vec![0.0f64; bins];
    for v in values {
        let cidx = (v - bin_min) / slope;
        let idxf = cidx.floor();
        let frac = cidx - idxf;
        let idx = idxf as isize;
        if frac == 0.0 {
            if idx >= 0 && (idx as usize) < bins {
                hist[idx as usize] += 1.0;
            }
        } else if idx >= 0 && (idx as usize) < bins - 1 {
            hist[idx as usize] += 1.0 - frac;
            hist[idx as usize + 1] += frac;
        }
    }
    Ok(sharpen_from_histogram(
        &hist,
        offset,
        padded,
        bins,
        bin_min,
        bin_max,
        slope,
        fwhm,
        wiener_noise,
    ))
}

#[allow(clippy::too_many_arguments)]
fn sharpen_from_histogram(
    hist: &[f64],
    offset: usize,
    padded: usize,
    bins: usize,
    bin_min: f64,
    bin_max: f64,
    slope: f64,
    fwhm: f64,
    wiener_noise: f64,
) -> SharpenLut {
    let mut vre = vec![0.0f64; padded];
    let mut vim = vec![0.0f64; padded];
    for (n, h) in hist.iter().enumerate() {
        vre[n + offset] = *h;
    }
    fft(&mut vre, &mut vim, false);

    let scaled_fwhm = fwhm / slope;
    let exp_factor = 4.0 * std::f64::consts::LN_2 / (scaled_fwhm * scaled_fwhm);
    let scale_factor = 2.0 * (std::f64::consts::LN_2 / std::f64::consts::PI).sqrt() / scaled_fwhm;
    let mut fre = vec![0.0f64; padded];
    let mut fim = vec![0.0f64; padded];
    fre[0] = scale_factor;
    let half = padded / 2;
    for n in 1..=half {
        let w = scale_factor * (-((n * n) as f64) * exp_factor).exp();
        fre[n] = w;
        fre[padded - n] = w;
    }
    if padded.is_multiple_of(2) {
        fre[half] = scale_factor * (0.25 * -((padded * padded) as f64) * exp_factor).exp();
    }
    fft(&mut fre, &mut fim, false);

    let mut u_re = vec![0.0f64; padded];
    let mut u_im = vec![0.0f64; padded];
    for n in 0..padded {
        let denom = fre[n] * fre[n] + fim[n] * fim[n] + wiener_noise;
        let gre = fre[n] / denom;
        u_re[n] = vre[n] * gre;
        u_im[n] = vim[n] * gre;
    }
    fft(&mut u_re, &mut u_im, true);
    for n in 0..padded {
        u_re[n] = u_re[n].max(0.0);
        u_im[n] = 0.0;
    }

    let mut num_re = vec![0.0f64; padded];
    let mut num_im = vec![0.0f64; padded];
    for n in 0..padded {
        num_re[n] = (bin_min + (n as f64 - offset as f64) * slope) * u_re[n];
    }
    fft(&mut num_re, &mut num_im, false);
    for n in 0..padded {
        let (a, b) = (num_re[n], num_im[n]);
        num_re[n] = a * fre[n] - b * fim[n];
        num_im[n] = a * fim[n] + b * fre[n];
    }
    fft(&mut num_re, &mut num_im, true);

    let mut den_re = u_re.clone();
    let mut den_im = u_im.clone();
    fft(&mut den_re, &mut den_im, false);
    for n in 0..padded {
        let (a, b) = (den_re[n], den_im[n]);
        den_re[n] = a * fre[n] - b * fim[n];
        den_im[n] = a * fim[n] + b * fre[n];
    }
    fft(&mut den_re, &mut den_im, true);

    let table: Vec<f64> = (0..bins)
        .map(|n| {
            let d = den_re[n + offset];
            if d != 0.0 {
                num_re[n + offset] / d
            } else {
                0.0
            }
        })
        .collect();
    SharpenLut {
        bin_min,
        bin_max,
        slope,
        table,
    }
}

/// Applies the sharpening table to one value (`SharpenImage`'s final lookup).
pub fn apply_sharpen_lut(lut: &SharpenLut, value: f64) -> f64 {
    let bins = lut.table.len();
    if bins == 0 || lut.slope <= 0.0 {
        return 0.0;
    }
    let cidx = (value - lut.bin_min) / lut.slope;
    let idxf = cidx.floor();
    if idxf < 0.0 {
        return lut.table[bins - 1];
    }
    let idx = idxf as usize;
    if idx < bins - 1 {
        lut.table[idx] + (lut.table[idx + 1] - lut.table[idx]) * (cidx - idx as f64)
    } else {
        lut.table[bins - 1]
    }
}

fn padded_size(bins: usize) -> usize {
    let mut p = 1usize;
    while p < bins {
        p <<= 1;
    }
    p << 1
}

/// In-place radix-2 FFT (`inverse` divides by `n`), `re`/`im` a power-of-two length.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    if n <= 1 {
        return;
    }
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let ang = 2.0 * std::f64::consts::PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let ur = re[i + k];
                let ui = im[i + k];
                let vr = re[i + k + len / 2] * cr - im[i + k + len / 2] * ci;
                let vi = re[i + k + len / 2] * ci + im[i + k + len / 2] * cr;
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + len / 2] = ur - vr;
                im[i + k + len / 2] = ui - vi;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
            i += len;
        }
        len <<= 1;
    }
    if inverse {
        for i in 0..n {
            re[i] /= n as f64;
            im[i] /= n as f64;
        }
    }
}

/// The coefficient-of-variation convergence value of `CalculateConvergenceMeasurement` over the
/// included voxels (`exp(field1 - field2)`).
pub fn convergence_measurement(field1: &[f64], field2: &[f64]) -> f64 {
    let mut mu = 0.0f64;
    let mut sigma = 0.0f64;
    let mut count = 0.0f64;
    for (a, b) in field1.iter().zip(field2) {
        let pixel = (a - b).exp();
        count += 1.0;
        if count > 1.0 {
            sigma += (pixel - mu).powi(2) * (count - 1.0) / count;
        }
        mu = mu * (1.0 - 1.0 / count) + pixel / count;
    }
    if count < 2.0 {
        return 0.0;
    }
    sigma.sqrt() / (count - 1.0).sqrt() / mu
}

/// Runs N4 on `input` at full resolution and returns the corrected image.
pub fn run_n4(input: &Volume<f64>, params: &N4Params) -> Result<N4Outcome> {
    params.validate()?;
    let full_grid = *input.grid();
    if (0..3).any(|d| full_grid.size()[d] < 2) {
        return Err(invalid(
            "n4",
            "the input needs at least two voxels per axis",
        ));
    }
    let f = params.shrink_factor.max(1);
    let shrunk = shrink_volume(input, [f, f, f])?;
    let shrunk_size = shrunk.grid().size();
    if (0..3).any(|d| shrunk_size[d] < 2) {
        return Err(invalid(
            "n4",
            "the shrunk grid needs at least two voxels per axis",
        ));
    }
    let log_input: Vec<f64> = shrunk
        .data()
        .iter()
        .map(|v| if *v > 0.0 { v.ln() } else { *v })
        .collect();

    let mut lattice: Option<ControlLattice> = None;
    let mut log_bias = vec![0.0f64; shrunk_size[0] * shrunk_size[1] * shrunk_size[2]];
    let mut levels = Vec::with_capacity(params.fitting_levels);
    for level in 0..params.fitting_levels {
        let cp = lattice
            .as_ref()
            .map(|l| l.size)
            .unwrap_or(params.number_of_control_points);
        let plan = FitPlan::new(shrunk_size, cp, params.spline_order)?;
        let limit = params.iterations[level];
        let mut trace = Vec::with_capacity(limit);
        let mut executed = 0;
        for _ in 0..limit {
            executed += 1;
            let log_uncorrected: Vec<f64> = log_input
                .iter()
                .zip(&log_bias)
                .map(|(a, b)| a - b)
                .collect();
            let lut = sharpen_lut(
                &log_uncorrected,
                params.number_of_histogram_bins,
                params.bias_field_fwhm,
                params.wiener_filter_noise,
            )?;
            let residual: Vec<f64> = log_uncorrected
                .iter()
                .map(|v| v - apply_sharpen_lut(&lut, *v))
                .collect();
            let phi = plan.phi(&residual)?;
            let updated = match lattice.take() {
                None => phi,
                Some(existing) => existing.add(&phi)?,
            };
            let new_log_bias = eval_lattice(&updated, shrunk_size, params.spline_order)?;
            let convergence = convergence_measurement(&log_bias, &new_log_bias);
            trace.push(convergence);
            lattice = Some(updated);
            log_bias = new_log_bias;
            if convergence <= params.convergence_threshold {
                break;
            }
        }
        levels.push(N4LevelTrace {
            level,
            iterations: executed,
            control_points: cp,
            grid_size: shrunk_size,
            convergence: trace,
        });
        if level + 1 < params.fitting_levels {
            let current = lattice
                .as_ref()
                .ok_or_else(|| invalid("n4", "no lattice"))?;
            lattice = Some(refine_lattice(current, params.spline_order)?);
        }
    }

    let lattice = lattice.ok_or_else(|| invalid("n4", "no lattice was produced"))?;
    let log_bias_full = eval_lattice(&lattice, full_grid.size(), params.spline_order)?;
    let corrected: Vec<f32> = input
        .data()
        .iter()
        .zip(&log_bias_full)
        .map(|(v, b)| (v / b.exp()) as f32)
        .collect();
    let bias_field: Vec<f32> = log_bias_full.iter().map(|b| b.exp() as f32).collect();
    Ok(N4Outcome {
        corrected: Volume::from_vec(full_grid, corrected)?,
        bias_field: Volume::from_vec(full_grid, bias_field)?,
        lattice,
        levels,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::linalg::IDENTITY3;

    fn grid(size: [usize; 3], spacing: f64) -> Grid {
        let origin = [
            -(size[0] as f64 - 1.0) * spacing / 2.0,
            -(size[1] as f64 - 1.0) * spacing / 2.0,
            -(size[2] as f64 - 1.0) * spacing / 2.0,
        ];
        Grid::new(size, [spacing; 3], origin, IDENTITY3).unwrap()
    }

    #[test]
    fn cubic_bspline_matches_hand_values() {
        assert!((cubic_bspline(0.0) - 2.0 / 3.0).abs() < 1e-15);
        assert!((cubic_bspline(1.0) - 1.0 / 6.0).abs() < 1e-15);
        assert!((cubic_bspline(0.5) - (4.0 - 1.5 + 0.375) / 6.0).abs() < 1e-15);
        assert!((cubic_bspline(1.5) - 0.125 / 6.0).abs() < 1e-15);
        assert_eq!(cubic_bspline(2.0), 0.0);
        assert_eq!(cubic_bspline(-3.0), 0.0);
        for &u in &[-1.7, -0.3, 0.3, 1.7] {
            assert!((cubic_bspline(u) - cubic_bspline(-u)).abs() < 1e-15);
        }
    }

    #[test]
    fn cubic_bspline_partition_of_unity() {
        for i in 0..40 {
            let frac = i as f64 / 40.0;
            let s: f64 = (0..4).map(|o| cubic_bspline(frac - o as f64 + 1.0)).sum();
            assert!((s - 1.0).abs() < 1e-12, "{frac}: {s}");
        }
    }

    #[test]
    fn refine_coefficients_are_the_cubic_subdivision_masks() {
        let (a, b) = refine_coefficients(3);
        assert_eq!(a, vec![0.5, 0.5, 0.0, 0.0]);
        assert_eq!(b, vec![0.125, 0.75, 0.125, 0.0]);
        for row in [&a, &b] {
            assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn refinement_preserves_the_spline_inside_the_domain() {
        let coarse = ControlLattice::from_vec(
            [5, 5, 5],
            (0..125).map(|v| (v as f64 * 0.13).sin()).collect(),
        )
        .unwrap();
        let fine = refine_lattice(&coarse, 3).unwrap();
        assert_eq!(fine.size, [7, 7, 7]);
        let out = [17, 19, 15];
        let a = eval_lattice(&coarse, out, 3).unwrap();
        let b = eval_lattice(&fine, out, 3).unwrap();
        let [nx, ny, nz] = out;
        let mut worst = 0.0f64;
        for k in 3..nz - 3 {
            for j in 3..ny - 3 {
                for i in 3..nx - 3 {
                    let lin = i + nx * (j + ny * k);
                    worst = worst.max((a[lin] - b[lin]).abs());
                }
            }
        }
        assert!(worst < 1e-10, "worst interior difference {worst}");
    }

    #[test]
    fn eval_of_a_constant_lattice_is_the_constant() {
        let lat = ControlLattice::from_vec([6, 6, 6], vec![2.5; 216]).unwrap();
        let out = eval_lattice(&lat, [9, 8, 7], 3).unwrap();
        for v in out {
            assert!((v - 2.5).abs() < 1e-12, "{v}");
        }
    }

    #[test]
    fn gather_converges_when_used_iteratively() {
        let gsize = [27, 23, 19];
        let target = eval_lattice(
            &ControlLattice::from_vec(
                [6, 6, 6],
                (0..216)
                    .map(|v| 0.3 * (((v % 7) as f64) - 3.0) + 0.1 * ((v % 5) as f64))
                    .collect(),
            )
            .unwrap(),
            gsize,
            3,
        )
        .unwrap();
        let plan = FitPlan::new(gsize, [6, 6, 6], 3).unwrap();
        let mut lattice = ControlLattice::zeros([6, 6, 6]);
        let mut worst = f64::MAX;
        for _ in 0..12 {
            let residual = eval_lattice(&lattice, gsize, 3).unwrap();
            let residual: Vec<f64> = target.iter().zip(&residual).map(|(t, r)| t - r).collect();
            let phi = plan.phi(&residual).unwrap();
            lattice = lattice.add(&phi).unwrap();
            let fitted = eval_lattice(&lattice, gsize, 3).unwrap();
            worst = target
                .iter()
                .zip(&fitted)
                .fold(0.0f64, |a, (t, f)| a.max((t - f).abs()));
        }
        assert!(worst < 0.15, "worst reconstruction error {worst}");
    }

    #[test]
    fn sharpening_table_is_finite_over_a_smooth_image() {
        let values: Vec<f64> = (0..2000)
            .map(|i| {
                let x = i as f64 * 0.01;
                (x + 1.0).ln() + 0.1 * x.sin()
            })
            .collect();
        let lut = sharpen_lut(&values, 200, 0.15, 0.01).unwrap();
        assert_eq!(lut.table.len(), 200);
        assert!(lut.table.iter().all(|v| v.is_finite()));
        let mid = apply_sharpen_lut(&lut, (lut.bin_min + lut.bin_max) * 0.5);
        assert!(mid.is_finite());
    }

    #[test]
    fn a_value_below_the_first_bin_takes_the_top_table_entry() {
        let lut = SharpenLut {
            bin_min: 0.0,
            bin_max: 3.0,
            slope: 1.0,
            table: vec![0.5, 1.5, 2.5, 3.5],
        };
        assert_eq!(apply_sharpen_lut(&lut, -0.4), 3.5);
        assert_eq!(apply_sharpen_lut(&lut, 0.5), 1.0);
        assert_eq!(apply_sharpen_lut(&lut, 9.0), 3.5);
    }

    #[test]
    fn histogram_range_follows_the_sequential_itk_rule() {
        assert_eq!(histogram_range(&[1.0, 5.0, 2.0, 0.0]), (0.0, 5.0));
        assert_eq!(histogram_range(&[0.0, 1.0, 2.0, 3.0]), (f64::MAX, 3.0));
    }

    #[test]
    fn levels_follow_the_itk_control_point_refinement_and_stay_finite() {
        let g = grid([32, 32, 24], 2.0);
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let p = g.index_to_point([i as f64, j as f64, k as f64]);
                let r =
                    ((p[0] / 30.0).powi(2) + (p[1] / 30.0).powi(2) + (p[2] / 22.0).powi(2)).sqrt();
                if r >= 1.0 {
                    0.0
                } else {
                    0.4 + 0.5 * (1.0 - r) + 0.05 * (p[0] / 5.0).sin()
                }
            })
            .collect();
        let input = Volume::from_vec(g, data).unwrap();
        let params = N4Params {
            shrink_factor: 2,
            iterations: vec![6; 4],
            ..N4Params::default()
        };
        let out = run_n4(&input, &params).unwrap();
        let cps: Vec<[usize; 3]> = out.levels.iter().map(|l| l.control_points).collect();
        assert_eq!(cps, vec![[4, 4, 4], [5, 5, 5], [7, 7, 7], [11, 11, 11]]);
        assert_eq!(out.levels[0].grid_size, [16, 16, 12]);
        assert!(out
            .bias_field
            .data()
            .iter()
            .all(|v| v.is_finite() && *v > 0.0));
        assert!(out.corrected.data().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn the_shrink_factor_is_the_same_on_every_axis_for_anisotropic_spacing() {
        let g = Grid::new([32, 32, 16], [1.0, 1.0, 4.0], [0.0; 3], IDENTITY3).unwrap();
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|lin| 1.0 + 0.001 * (lin % 97) as f64)
            .collect();
        let input = Volume::from_vec(g, data).unwrap();
        let params = N4Params {
            shrink_factor: 4,
            iterations: vec![2; 2],
            fitting_levels: 2,
            ..N4Params::default()
        };
        let out = run_n4(&input, &params).unwrap();
        assert_eq!(out.levels[0].grid_size, [8, 8, 4]);
    }

    #[test]
    fn a_constant_image_is_left_unchanged() {
        let g = grid([20, 18, 16], 2.0);
        let input = Volume::from_vec(g, vec![7.5f64; g.num_voxels()]).unwrap();
        let params = N4Params {
            shrink_factor: 2,
            iterations: vec![4; 4],
            ..N4Params::default()
        };
        let out = run_n4(&input, &params).unwrap();
        for (c, b) in out.corrected.data().iter().zip(out.bias_field.data()) {
            assert!((f64::from(*c) - 7.5).abs() < 1e-4, "corrected {c}");
            assert!((f64::from(*b) - 1.0).abs() < 1e-6, "bias {b}");
        }
    }

    #[test]
    fn run_is_bitwise_deterministic() {
        let g = grid([24, 24, 16], 2.0);
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                1.0 + 0.5 * ((i * 3 + j * 5 + k * 7) as f64 * 0.01).sin().abs()
            })
            .collect();
        let input = Volume::from_vec(g, data).unwrap();
        let params = N4Params {
            shrink_factor: 2,
            iterations: vec![5; 4],
            ..N4Params::default()
        };
        let a = run_n4(&input, &params).unwrap();
        let b = run_n4(&input, &params).unwrap();
        assert_eq!(a.corrected.data(), b.corrected.data());
        assert_eq!(a.lattice, b.lattice);
    }
}
