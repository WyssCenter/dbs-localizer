// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
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
// Portions derived from ITK `itkComposeDisplacementFieldsImageFilter.hxx` (`DynamicThreadedGenerateData`),
// `itkInvertDisplacementFieldImageFilter.hxx` (`GenerateData`, `DynamicThreadedGenerateData`),
// `itkGaussianOperator.hxx` (`GenerateCoefficients`, `ModifiedBesselI0`, `ModifiedBesselI1`,
// `ModifiedBesselI`), `itkNeighborhoodOperator.hxx` (`CreateDirectional`),
// `itkVectorNeighborhoodOperatorImageFilter.hxx` (zero-flux Neumann boundary),
// `itkVectorLinearInterpolateImageFunction.hxx` (`EvaluateAtContinuousIndex`),
// `itkDisplacementFieldTransformParametersAdaptor.hxx` (`AdaptTransformParameters`) and from
// `itkSyNImageRegistrationMethod.hxx` (`GaussianSmoothDisplacementField`, `ScaleUpdateField`,
// `InvertDisplacementField`). See `docs/THIRD_PARTY_PORTS.md`.

//! Dense displacement-field operations of ITK's SyN registration, in `f64`: composition,
//! fixed-point inversion, discrete Gaussian smoothing with a pinned boundary, max-norm scaling
//! and resampling between grids.
//!
//! All fields live on a [`Grid`] and store LPS millimetre displacements. Reductions sum in a
//! fixed chunk order so results do not depend on the number of threads.

use rayon::prelude::*;

use crate::error::{invalid, Result};
use crate::grid::Grid;
use crate::interp::linear_at_index;
use crate::linalg::Vec3;
use crate::transform::DisplacementField;
use crate::volume::Volume;

const REDUCE_CHUNK: usize = 4096;

/// Sums `values` with a fixed chunk partition, so the result is independent of thread count.
pub fn ordered_sum(values: &[f64]) -> f64 {
    let partial: Vec<f64> = values
        .par_chunks(REDUCE_CHUNK)
        .map(|c| c.iter().sum::<f64>())
        .collect();
    partial.iter().sum()
}

/// A displacement field on a [`Grid`] with `f64` storage, `(dx, dy, dz)` per voxel in LPS
/// millimetres, x-fastest. The transform it describes maps `p` to `p + u(p)`.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorField {
    grid: Grid,
    data: Vec<Vec3>,
}

impl VectorField {
    /// A zero field.
    pub fn zeros(grid: Grid) -> Self {
        let n = grid.num_voxels();
        Self {
            grid,
            data: vec![[0.0; 3]; n],
        }
    }

    /// Wraps per-voxel vectors; fails on a length mismatch.
    pub fn from_vec(grid: Grid, data: Vec<Vec3>) -> Result<Self> {
        if data.len() != grid.num_voxels() {
            return Err(crate::error::RegError::Length {
                what: "vector field data",
                expected: grid.num_voxels(),
                got: data.len(),
            });
        }
        Ok(Self { grid, data })
    }

    /// A field sampled from `f(physical point)` at every voxel centre.
    pub fn from_fn(grid: Grid, f: impl Fn(Vec3) -> Vec3 + Sync) -> Self {
        let data: Vec<Vec3> = (0..grid.num_voxels())
            .into_par_iter()
            .map(|idx| {
                let [i, j, k] = grid.ijk(idx);
                f(grid.index_to_point([i as f64, j as f64, k as f64]))
            })
            .collect();
        Self { grid, data }
    }

    /// Widens an `f32` displacement field.
    pub fn from_displacement_field(f: &DisplacementField) -> Self {
        let data = f
            .data()
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| c.map(f64::from))
            .collect();
        Self {
            grid: *f.grid(),
            data,
        }
    }

    /// Rounds to an `f32` displacement field (the on-disk precision of ANTs `--float 1`).
    pub fn to_displacement_field(&self) -> Result<DisplacementField> {
        let mut out = Vec::with_capacity(self.data.len() * 3);
        for v in &self.data {
            out.extend([v[0] as f32, v[1] as f32, v[2] as f32]);
        }
        DisplacementField::new(self.grid, out)
    }

    /// The grid.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Per-voxel vectors, x-fastest.
    pub fn data(&self) -> &[Vec3] {
        &self.data
    }

    /// Mutable per-voxel vectors.
    pub fn data_mut(&mut self) -> &mut [Vec3] {
        &mut self.data
    }

    /// ITK `VectorLinearInterpolateImageFunction` at a continuous index (no bounds check;
    /// neighbours are clamped to the buffer).
    pub fn interpolate_index(&self, c: Vec3) -> Vec3 {
        let size = self.grid.size();
        let mut idx = [[0usize; 2]; 3];
        let mut w = [[0.0f64; 2]; 3];
        for a in 0..3 {
            let f = c[a].floor();
            let d = c[a] - f;
            let base = f as i64;
            let last = size[a] as i64 - 1;
            idx[a][0] = base.clamp(0, last) as usize;
            idx[a][1] = (base + 1).clamp(0, last) as usize;
            w[a] = [1.0 - d, d];
        }
        let mut out = [0.0f64; 3];
        for counter in 0..8usize {
            let bx = counter & 1;
            let by = (counter >> 1) & 1;
            let bz = (counter >> 2) & 1;
            let overlap = w[0][bx] * w[1][by] * w[2][bz];
            if overlap != 0.0 {
                let v = &self.data[self.grid.linear_index(idx[0][bx], idx[1][by], idx[2][bz])];
                out[0] += overlap * v[0];
                out[1] += overlap * v[1];
                out[2] += overlap * v[2];
            }
        }
        out
    }

    /// Interpolated displacement at a physical point; `None` outside the ITK buffer
    /// (continuous index outside `[-0.5, size - 0.5)`).
    pub fn interpolate(&self, p: Vec3) -> Option<Vec3> {
        let c = self.grid.point_to_index(p);
        self.grid
            .contains_continuous_index(c)
            .then(|| self.interpolate_index(c))
    }

    /// Whether voxel `linear` lies on a face of the grid.
    pub fn is_boundary(&self, linear: usize) -> bool {
        let [i, j, k] = self.grid.ijk(linear);
        let [nx, ny, nz] = self.grid.size();
        i == 0 || j == 0 || k == 0 || i == nx - 1 || j == ny - 1 || k == nz - 1
    }

    /// Largest `|u / spacing|` over all voxels (ITK `ScaleUpdateField`'s `maxNorm`).
    pub fn max_scaled_norm(&self) -> f64 {
        let s = self.grid.spacing();
        self.data.par_iter().map(|v| scaled_norm(*v, s)).reduce(
            || 0.0,
            |a, b| {
                if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else {
                    a.max(b)
                }
            },
        )
    }

    /// Euclidean norm of the whole field viewed as one vector (vnl `magnitude`).
    pub fn l2_norm(&self) -> f64 {
        let sq: Vec<f64> = self
            .data
            .par_iter()
            .map(|v| v[0] * v[0] + v[1] * v[1] + v[2] * v[2])
            .collect();
        ordered_sum(&sq).sqrt()
    }

    /// `self += a * x`; fails when the grids differ.
    pub fn axpy(&mut self, a: f64, x: &VectorField) -> Result<()> {
        if self.grid != x.grid {
            return Err(invalid("field axpy", "grids differ"));
        }
        self.data
            .par_iter_mut()
            .zip(x.data.par_iter())
            .for_each(|(s, v)| {
                s[0] += a * v[0];
                s[1] += a * v[1];
                s[2] += a * v[2];
            });
        Ok(())
    }

    /// `self *= s`.
    pub fn scale(&mut self, s: f64) {
        self.data.par_iter_mut().for_each(|v| {
            v[0] *= s;
            v[1] *= s;
            v[2] *= s;
        });
    }
}

fn scaled_norm(v: Vec3, spacing: Vec3) -> f64 {
    let a = v[0] / spacing[0];
    let b = v[1] / spacing[1];
    let c = v[2] / spacing[2];
    (a * a + b * b + c * c).sqrt()
}

/// ITK `ComposeDisplacementFieldsImageFilter`: `out(x) = w(x) + u(x + w(x))` with `w` the
/// warping field and `u` the displacement field, interpolated linearly and taken as zero
/// when `x + w(x)` lies outside `u`'s buffer. Both fields must share a grid.
pub fn compose_displacement_fields(
    displacement: &VectorField,
    warping: &VectorField,
) -> Result<VectorField> {
    if displacement.grid != warping.grid {
        return Err(invalid("compose", "displacement and warping grids differ"));
    }
    let grid = warping.grid;
    let data: Vec<Vec3> = warping
        .data
        .par_iter()
        .enumerate()
        .map(|(idx, w)| {
            let [i, j, k] = grid.ijk(idx);
            let p1 = grid.index_to_point([i as f64, j as f64, k as f64]);
            let p2 = [p1[0] + w[0], p1[1] + w[1], p1[2] + w[2]];
            let d = displacement.interpolate(p2).unwrap_or([0.0; 3]);
            [w[0] + d[0], w[1] + d[1], w[2] + d[2]]
        })
        .collect();
    Ok(VectorField { grid, data })
}

/// Parameters of ITK's `InvertDisplacementFieldImageFilter` as SyN sets them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InvertParams {
    /// `SetMaximumNumberOfIterations` (SyN: 20).
    pub max_iterations: u32,
    /// `SetMeanErrorToleranceThreshold` (SyN: 0.001), in voxels.
    pub mean_error_tolerance: f64,
    /// `SetMaxErrorToleranceThreshold` (SyN: 0.1), in voxels.
    pub max_error_tolerance: f64,
    /// `EnforceBoundaryCondition` (ITK default and SyN: true): boundary voxels of the
    /// inverse are set to zero after every update.
    pub enforce_boundary_condition: bool,
}

impl InvertParams {
    /// The values `SyNImageRegistrationMethod::InvertDisplacementField` passes.
    pub const SYN: InvertParams = InvertParams {
        max_iterations: 20,
        mean_error_tolerance: 0.001,
        max_error_tolerance: 0.1,
        enforce_boundary_condition: true,
    };
}

/// What ITK's inversion filter reports after a run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InvertStats {
    /// Loop bodies executed (each measures the error of the current estimate, then updates it).
    pub iterations: u32,
    /// `MeanErrorNorm` of the last measurement, in voxels (before the last update).
    pub mean_error_norm: f64,
    /// `MaxErrorNorm` of the last measurement, in voxels (before the last update).
    pub max_error_norm: f64,
}

/// ITK `InvertDisplacementFieldImageFilter`: fixed-point iteration for the inverse of `field`
/// starting from `initial` (zero when `None`). The loop runs while
/// `iteration < max_iterations && max_error > max_tolerance && mean_error > mean_tolerance`
/// where the errors are those measured in the previous body; every body measures
/// `e = psi + field(x + psi)` (scaled by the inverse spacing), then updates
/// `psi += eps * clamp(-e)` with `eps = 0.75` on the first body and `0.5` afterwards.
pub fn invert_displacement_field(
    field: &VectorField,
    initial: Option<&VectorField>,
    params: &InvertParams,
) -> Result<(VectorField, InvertStats)> {
    let grid = field.grid;
    let mut inverse = match initial {
        Some(init) => {
            if init.grid != grid {
                return Err(invalid("invert", "initial estimate grid differs"));
            }
            init.clone()
        }
        None => VectorField::zeros(grid),
    };
    let spacing = grid.spacing();
    let count = grid.num_voxels() as f64;
    let mut max_error = f64::MAX;
    let mut mean_error = f64::MAX;
    let mut iteration = 0u32;
    let mut executed = 0u32;
    loop {
        let current = iteration;
        iteration += 1;
        if !(current < params.max_iterations
            && max_error > params.max_error_tolerance
            && mean_error > params.mean_error_tolerance)
        {
            break;
        }
        executed += 1;
        let composed = compose_displacement_fields(field, &inverse)?;
        let norms: Vec<f64> = composed
            .data
            .par_iter()
            .map(|e| scaled_norm(*e, spacing))
            .collect();
        mean_error = ordered_sum(&norms) / count;
        max_error = norms.par_iter().copied().reduce(|| 0.0, f64::max);
        let epsilon = if current == 0 { 0.75 } else { 0.5 };
        let limit = epsilon * max_error;
        let enforce = params.enforce_boundary_condition;
        let [nx, ny, nz] = grid.size();
        let boundary = |idx: usize| -> bool {
            let [i, j, k] = grid.ijk(idx);
            i == 0 || j == 0 || k == 0 || i == nx - 1 || j == ny - 1 || k == nz - 1
        };
        inverse
            .data
            .par_iter_mut()
            .zip(composed.data.par_iter())
            .zip(norms.par_iter())
            .enumerate()
            .for_each(|(idx, ((psi, e), n))| {
                let mut update = [-e[0], -e[1], -e[2]];
                if *n > limit {
                    let f = limit / *n;
                    update = [update[0] * f, update[1] * f, update[2] * f];
                }
                *psi = [
                    psi[0] + update[0] * epsilon,
                    psi[1] + update[1] * epsilon,
                    psi[2] + update[2] * epsilon,
                ];
                if enforce && boundary(idx) {
                    *psi = [0.0; 3];
                }
            });
    }
    Ok((
        inverse,
        InvertStats {
            iterations: executed,
            mean_error_norm: if executed == 0 { 0.0 } else { mean_error },
            max_error_norm: if executed == 0 { 0.0 } else { max_error },
        },
    ))
}

/// ITK `GaussianOperator::ModifiedBesselI0` (polynomial approximation, Abramowitz and Stegun).
pub fn bessel_i0(y: f64) -> f64 {
    let d = y.abs();
    if d < 3.75 {
        let mut m = y / 3.75;
        m *= m;
        1.0 + m
            * (3.5156229
                + m * (3.0899424
                    + m * (1.2067492 + m * (0.2659732 + m * (0.360768e-1 + m * 0.45813e-2)))))
    } else {
        let m = 3.75 / d;
        (d.exp() / d.sqrt())
            * (0.39894228
                + m * (0.1328592e-1
                    + m * (0.225319e-2
                        + m * (-0.157565e-2
                            + m * (0.916281e-2
                                + m * (-0.2057706e-1
                                    + m * (0.2635537e-1
                                        + m * (-0.1647633e-1 + m * 0.392377e-2))))))))
    }
}

/// ITK `GaussianOperator::ModifiedBesselI1`.
pub fn bessel_i1(y: f64) -> f64 {
    let d = y.abs();
    let accumulator = if d < 3.75 {
        let mut m = y / 3.75;
        m *= m;
        d * (0.5
            + m * (0.87890594
                + m * (0.51498869
                    + m * (0.15084934 + m * (0.2658733e-1 + m * (0.301532e-2 + m * 0.32411e-3))))))
    } else {
        let m = 3.75 / d;
        let a = 0.2282967e-1 + m * (-0.2895312e-1 + m * (0.1787654e-1 - m * 0.420059e-2));
        let a = 0.39894228
            + m * (-0.3988024e-1
                + m * (-0.362018e-2 + m * (0.163801e-2 + m * (-0.1031555e-1 + m * a))));
        a * (d.exp() / d.sqrt())
    };
    if y < 0.0 {
        -accumulator
    } else {
        accumulator
    }
}

/// ITK `GaussianOperator::ModifiedBesselI` for `n >= 2`: Miller's downward recurrence
/// normalised with [`bessel_i0`]. Returns `0` for `y == 0`.
pub fn bessel_i(n: i32, y: f64) -> f64 {
    const ACCURACY: f64 = 40.0;
    if y == 0.0 {
        return 0.0;
    }
    let toy = 2.0 / y.abs();
    let mut qip = 0.0f64;
    let mut accumulator = 0.0f64;
    let mut qi = 1.0f64;
    let start = 2 * (n + (ACCURACY * f64::from(n)).sqrt() as i32);
    let mut j = start;
    while j > 0 {
        let qim = qip + f64::from(j) * toy * qi;
        qip = qi;
        qi = qim;
        if qi.abs() > 1.0e10 {
            accumulator *= 1.0e-10;
            qi *= 1.0e-10;
            qip *= 1.0e-10;
        }
        if j == n {
            accumulator = qip;
        }
        j -= 1;
    }
    accumulator *= bessel_i0(y) / qi;
    if y < 0.0 && (n & 1) == 1 {
        -accumulator
    } else {
        accumulator
    }
}

/// The full symmetric tap vector (`2 r + 1` values summing to one) of ITK's `GaussianOperator`
/// for `variance` in squared voxels: `c_n = exp(-t) I_n(t)` until the one-sided sum reaches
/// `1 - max_error` or the half kernel exceeds `max_width`, then normalised.
pub fn gaussian_operator_coefficients(variance: f64, max_error: f64, max_width: usize) -> Vec<f64> {
    let et = (-variance).exp();
    let cap = 1.0 - max_error;
    let mut half = vec![et * bessel_i0(variance)];
    let mut sum = half[0];
    half.push(et * bessel_i1(variance));
    sum += half[1] * 2.0;
    let mut i = 2usize;
    while sum < cap {
        half.push(et * bessel_i(i as i32, variance));
        sum += half[i] * 2.0;
        if half[i] <= 0.0 {
            break;
        }
        if half.len() > max_width {
            break;
        }
        i += 1;
    }
    for c in &mut half {
        *c /= sum;
    }
    let mut full: Vec<f64> = half.iter().skip(1).rev().copied().collect();
    full.extend(half.iter().copied());
    full
}

fn convolve_axis(data: &[Vec3], size: [usize; 3], axis: usize, kernel: &[f64]) -> Vec<Vec3> {
    let radius = (kernel.len() / 2) as i64;
    let stride = [1, size[0], size[0] * size[1]][axis];
    let n = size[axis] as i64;
    (0..data.len())
        .into_par_iter()
        .map(|idx| {
            let pos = ((idx / stride) % size[axis]) as i64;
            let base = idx - pos as usize * stride;
            let mut acc = [0.0f64; 3];
            for (t, w) in kernel.iter().enumerate() {
                let q = (pos + t as i64 - radius).clamp(0, n - 1) as usize;
                let v = &data[base + q * stride];
                acc[0] += w * v[0];
                acc[1] += w * v[1];
                acc[2] += w * v[2];
            }
            acc
        })
        .collect()
}

/// ITK SyN's `GaussianSmoothDisplacementField`: separable smoothing along x, y, z with
/// [`gaussian_operator_coefficients`] (`variance` in squared voxels of the field's own grid,
/// `max_error` 0.001, maximum width the axis size, edge replication), then `smooth * w1 +
/// original * w2` with `w1 = 1` for `variance >= 0.5` and `1 - variance / 0.5` below, and every
/// voxel on a grid face set to zero. For `variance <= 0` the field is returned unchanged
/// (faces not zeroed).
pub fn gaussian_smooth_displacement_field(field: &VectorField, variance: f64) -> VectorField {
    if variance <= 0.0 {
        return field.clone();
    }
    let size = field.grid.size();
    let mut current: Vec<Vec3> = field.data.clone();
    for axis in 0..3 {
        let kernel = gaussian_operator_coefficients(variance, 0.001, size[axis]);
        current = convolve_axis(&current, size, axis, &kernel);
    }
    let weight1 = if variance < 0.5 {
        1.0 - variance / 0.5
    } else {
        1.0
    };
    let weight2 = 1.0 - weight1;
    let grid = field.grid;
    let [nx, ny, nz] = size;
    current
        .par_iter_mut()
        .zip(field.data.par_iter())
        .enumerate()
        .for_each(|(idx, (s, f))| {
            let [i, j, k] = grid.ijk(idx);
            if i == 0 || j == 0 || k == 0 || i == nx - 1 || j == ny - 1 || k == nz - 1 {
                *s = [0.0; 3];
            } else {
                *s = [
                    s[0] * weight1 + f[0] * weight2,
                    s[1] * weight1 + f[1] * weight2,
                    s[2] * weight1 + f[2] * weight2,
                ];
            }
        });
    VectorField {
        grid,
        data: current,
    }
}

/// ITK `DisplacementFieldTransformParametersAdaptor::AdaptTransformParameters`: the field
/// resampled onto `grid` with a linear interpolator in physical space, identity transform and
/// default value zero outside the old buffer; displacement values are not rescaled. Returns a
/// copy when the grids are identical.
pub fn resample_field(field: &VectorField, grid: &Grid) -> Result<VectorField> {
    if field.grid == *grid {
        return Ok(field.clone());
    }
    let comps: Vec<Volume<f64>> = (0..3)
        .map(|c| Volume::from_vec(field.grid, field.data.iter().map(|v| v[c]).collect()))
        .collect::<Result<_>>()?;
    let src = field.grid;
    let data: Vec<Vec3> = (0..grid.num_voxels())
        .into_par_iter()
        .map(|idx| {
            let [i, j, k] = grid.ijk(idx);
            let p = grid.index_to_point([i as f64, j as f64, k as f64]);
            let c = src.point_to_index(p);
            if !src.contains_continuous_index(c) {
                return [0.0; 3];
            }
            [
                linear_at_index(&comps[0], c),
                linear_at_index(&comps[1], c),
                linear_at_index(&comps[2], c),
            ]
        })
        .collect();
    Ok(VectorField { grid: *grid, data })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_range_loop)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    #[test]
    fn max_scaled_norm_propagates_a_non_finite_voxel() {
        let g = Grid::new([5, 4, 3], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let mut f = VectorField::from_fn(g, |p| [p[0], 0.0, 0.0]);
        assert_eq!(f.max_scaled_norm(), 4.0);
        f.data[17] = [f64::NAN, 0.0, 0.0];
        assert!(f.max_scaled_norm().is_nan());
    }

    fn grid(n: [usize; 3], spacing: Vec3) -> Grid {
        Grid::new(n, spacing, [0.0; 3], IDENTITY3).unwrap()
    }

    fn series_i(n: u32, t: f64) -> f64 {
        let mut sum = 0.0;
        let mut term = (t / 2.0).powi(n as i32);
        for k in 0..n {
            term /= f64::from(k + 1);
        }
        for k in 0..60u32 {
            sum += term;
            term *= (t / 2.0) * (t / 2.0) / (f64::from(k + 1) * f64::from(k + 1 + n));
        }
        sum
    }

    fn sine_field(g: Grid, amp: f64) -> VectorField {
        let [nx, ny, nz] = g.size();
        let s = g.spacing();
        VectorField::from_fn(g, |p| {
            let (x, y, z) = (
                p[0] / ((nx as f64 - 1.0) * s[0]),
                p[1] / ((ny as f64 - 1.0) * s[1]),
                p[2] / ((nz as f64 - 1.0) * s[2]),
            );
            let w = (std::f64::consts::PI * x).sin()
                * (std::f64::consts::PI * y).sin()
                * (std::f64::consts::PI * z).sin();
            [amp * w, -0.7 * amp * w, 0.5 * amp * w]
        })
    }

    #[test]
    fn operator_taps_match_the_bessel_series_definition() {
        for variance in [0.5, 1.0, 3.0, 8.0] {
            let taps = gaussian_operator_coefficients(variance, 0.001, 1000);
            let r = taps.len() / 2;
            assert_eq!(taps.len(), 2 * r + 1);
            let sum: f64 = taps.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12);
            for t in 0..taps.len() {
                assert!((taps[t] - taps[taps.len() - 1 - t]).abs() < 1e-15);
            }
            let raw: Vec<f64> = (0..=r)
                .map(|n| (-variance).exp() * series_i(n as u32, variance))
                .collect();
            let one_sided: f64 = raw[0] + 2.0 * raw[1..].iter().sum::<f64>();
            assert!(one_sided >= 0.999 - 1e-5);
            let without_last: f64 = raw[0] + 2.0 * raw[1..r].iter().sum::<f64>();
            assert!(without_last < 0.999 + 1e-5);
            for n in 0..=r {
                assert!(
                    (taps[r + n] - raw[n] / one_sided).abs() < 5e-6,
                    "variance {variance} tap {n}: {} vs {}",
                    taps[r + n],
                    raw[n] / one_sided
                );
            }
        }
    }

    #[test]
    fn variance_three_kernel_has_the_expected_radius_and_centre() {
        let taps = gaussian_operator_coefficients(3.0, 0.001, 1000);
        let r = taps.len() / 2;
        assert_eq!(r, 6);
        let raw: Vec<f64> = (0..=r)
            .map(|n| (-3.0f64).exp() * series_i(n as u32, 3.0))
            .collect();
        let total = raw[0] + 2.0 * raw[1..].iter().sum::<f64>();
        assert!((taps[r] - raw[0] / total).abs() < 1e-6);
        assert!((taps[r] - 0.2433).abs() < 5e-4);
    }

    #[test]
    fn maximum_width_truncates_the_kernel() {
        let taps = gaussian_operator_coefficients(3.0, 0.001, 3);
        let r = taps.len() / 2;
        assert_eq!(r, 3);
        assert!((taps.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn bessel_polynomials_match_the_series_on_both_branches() {
        for y in [0.3, 1.0, 3.0, 3.74, 3.76, 6.0, 12.0] {
            let (i0, i1) = (series_i(0, y), series_i(1, y));
            assert!((bessel_i0(y) - i0).abs() < 2e-7 * i0.max(1.0), "I0({y})");
            assert!((bessel_i1(y) - i1).abs() < 2e-7 * i1.max(1.0), "I1({y})");
            let i3 = series_i(3, y);
            assert!((bessel_i(3, y) - i3).abs() < 2e-6 * i3.max(1e-3), "I3({y})");
        }
        assert_eq!(bessel_i(2, 0.0), 0.0);
        assert!((bessel_i1(-2.0) + bessel_i1(2.0)).abs() < 1e-15);
    }

    #[test]
    fn smoothing_a_delta_returns_the_kernel_and_pins_the_boundary() {
        let g = grid([21, 21, 21], [1.0; 3]);
        let mut f = VectorField::zeros(g);
        let c = g.linear_index(10, 10, 10);
        f.data_mut()[c] = [1.0, 0.0, 0.0];
        let s = gaussian_smooth_displacement_field(&f, 3.0);
        let taps = gaussian_operator_coefficients(3.0, 0.001, 21);
        let r = taps.len() / 2;
        for t in 0..taps.len() {
            let i = 10 + t - r;
            let want = taps[t] * taps[r] * taps[r];
            let got = s.data()[g.linear_index(i, 10, 10)][0];
            assert!((got - want).abs() < 1e-14, "tap {t}");
        }
        for idx in 0..g.num_voxels() {
            if s.is_boundary(idx) {
                assert_eq!(s.data()[idx], [0.0; 3]);
            }
        }
    }

    #[test]
    fn smoothing_preserves_a_constant_in_the_interior_and_blends_below_half_variance() {
        let g = grid([15, 15, 15], [1.0; 3]);
        let f = VectorField::from_fn(g, |_| [2.0, -1.0, 0.5]);
        let s = gaussian_smooth_displacement_field(&f, 3.0);
        let mid = s.data()[g.linear_index(7, 7, 7)];
        assert!((mid[0] - 2.0).abs() < 1e-12 && (mid[1] + 1.0).abs() < 1e-12);
        let kernel = gaussian_operator_coefficients(0.25, 0.001, 15);
        let s2 = gaussian_smooth_displacement_field(&f, 0.25);
        let mid2 = s2.data()[g.linear_index(7, 7, 7)];
        assert!((mid2[0] - 2.0).abs() < 1e-12);
        assert!(kernel.len() >= 3);
        let same = gaussian_smooth_displacement_field(&f, 0.0);
        assert_eq!(same, f);
        assert_eq!(same.data()[0], [2.0, -1.0, 0.5]);
    }

    #[test]
    fn compose_with_constants_adds_them_and_outside_the_buffer_the_update_is_zero() {
        let g = grid([8, 8, 8], [2.0; 3]);
        let u = VectorField::from_fn(g, |_| [1.0, 2.0, 3.0]);
        let w = VectorField::from_fn(g, |_| [0.5, 0.0, -0.5]);
        let c = compose_displacement_fields(&u, &w).unwrap();
        for v in c.data() {
            assert_eq!(*v, [1.5, 2.0, 2.5]);
        }
        let far = VectorField::from_fn(g, |_| [100.0, 0.0, 0.0]);
        let c = compose_displacement_fields(&u, &far).unwrap();
        for v in c.data() {
            assert_eq!(*v, [100.0, 0.0, 0.0]);
        }
        let other = VectorField::zeros(grid([8, 8, 9], [2.0; 3]));
        assert!(compose_displacement_fields(&u, &other).is_err());
    }

    #[test]
    fn compose_follows_the_warping_field_for_a_linear_displacement() {
        let g = grid([10, 10, 10], [1.0; 3]);
        let u = VectorField::from_fn(g, |p| [0.1 * p[0], 0.0, 0.0]);
        let w = VectorField::from_fn(g, |_| [1.0, 0.0, 0.0]);
        let c = compose_displacement_fields(&u, &w).unwrap();
        let v = c.data()[g.linear_index(3, 4, 4)];
        assert!((v[0] - (1.0 + 0.1 * 4.0)).abs() < 1e-12);
    }

    #[test]
    fn inverse_of_a_smooth_field_composes_to_identity_within_the_filter_tolerance() {
        let g = grid([24, 24, 24], [1.5, 1.5, 1.5]);
        let u = sine_field(g, 2.0);
        let (inv, stats) = invert_displacement_field(&u, None, &InvertParams::SYN).unwrap();
        assert!(stats.iterations >= 2 && stats.iterations <= 20);
        assert!(
            stats.max_error_norm <= 0.1 || stats.mean_error_norm <= 0.001 || stats.iterations == 20
        );
        let residual = compose_displacement_fields(&u, &inv).unwrap();
        let mut max_interior = 0.0f64;
        for idx in 0..g.num_voxels() {
            if !residual.is_boundary(idx) {
                max_interior = max_interior.max(scaled_norm(residual.data()[idx], g.spacing()));
            }
        }
        assert!(max_interior < 0.1, "interior residual {max_interior}");
        for idx in 0..g.num_voxels() {
            if inv.is_boundary(idx) {
                assert_eq!(inv.data()[idx], [0.0; 3]);
            }
        }
        let (again, s2) = invert_displacement_field(&u, Some(&inv), &InvertParams::SYN).unwrap();
        assert_eq!(s2.iterations, 1);
        assert!(s2.max_error_norm < 0.1);
        let diff = again
            .data()
            .iter()
            .zip(inv.data())
            .map(|(a, b)| (a[0] - b[0]).abs())
            .fold(0.0, f64::max);
        assert!(diff < 0.05);
    }

    #[test]
    fn inversion_stops_at_the_iteration_limit_and_reports_the_pre_update_error() {
        let g = grid([12, 12, 12], [1.0; 3]);
        let u = sine_field(g, 3.0);
        let params = InvertParams {
            max_iterations: 3,
            mean_error_tolerance: 0.0,
            max_error_tolerance: 0.0,
            enforce_boundary_condition: true,
        };
        let (_, stats) = invert_displacement_field(&u, None, &params).unwrap();
        assert_eq!(stats.iterations, 3);
        let first = invert_displacement_field(
            &u,
            None,
            &InvertParams {
                max_iterations: 1,
                ..params
            },
        )
        .unwrap()
        .1;
        assert!(first.max_error_norm > stats.max_error_norm);
        let zero = VectorField::zeros(g);
        let (inv, s) = invert_displacement_field(&zero, None, &InvertParams::SYN).unwrap();
        assert_eq!(s.iterations, 1);
        assert!(inv.data().iter().all(|v| *v == [0.0; 3]));
    }

    #[test]
    fn resampling_reproduces_a_linear_field_and_is_zero_outside_the_old_buffer() {
        let coarse = grid([8, 8, 8], [2.0; 3]);
        let f = VectorField::from_fn(coarse, |p| [0.1 * p[0] + 1.0, 0.2 * p[1], -0.05 * p[2]]);
        let fine = Grid::new([13, 13, 13], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let r = resample_field(&f, &fine).unwrap();
        let v = r.data()[fine.linear_index(5, 6, 7)];
        assert!((v[0] - (0.1 * 5.0 + 1.0)).abs() < 1e-12);
        assert!((v[1] - 0.2 * 6.0).abs() < 1e-12);
        let wide = Grid::new([8, 8, 8], [2.0; 3], [-20.0, 0.0, 0.0], IDENTITY3).unwrap();
        let r = resample_field(&f, &wide).unwrap();
        assert_eq!(r.data()[wide.linear_index(0, 3, 3)], [0.0; 3]);
        assert_eq!(resample_field(&f, &coarse).unwrap(), f);
    }

    #[test]
    fn displacement_field_round_trip_rounds_to_f32() {
        let g = grid([3, 3, 3], [1.0; 3]);
        let f = VectorField::from_fn(g, |p| [p[0] * 0.1, 0.0, 1e-9]);
        let d = f.to_displacement_field().unwrap();
        let back = VectorField::from_displacement_field(&d);
        for (a, b) in f.data().iter().zip(back.data()) {
            assert!((a[0] - b[0]).abs() < 1e-7);
        }
        assert!(VectorField::from_vec(g, vec![[0.0; 3]; 4]).is_err());
    }

    #[test]
    fn reductions_are_independent_of_the_data_layout_of_threads() {
        let values: Vec<f64> = (0..100_000)
            .map(|i| ((i * 7919) % 1000) as f64 * 1e-3)
            .collect();
        let a = ordered_sum(&values);
        let b = ordered_sum(&values);
        assert_eq!(a.to_bits(), b.to_bits());
        let seq: f64 = values
            .chunks(REDUCE_CHUNK)
            .map(|c| c.iter().sum::<f64>())
            .sum();
        assert_eq!(a.to_bits(), seq.to_bits());
    }
}
