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
// Portions derived from ITK `itkShrinkImageFilter.hxx` (`GenerateOutputInformation`,
// `DynamicThreadedGenerateData`), `itkSmoothingRecursiveGaussianImageFilter.hxx` (axis order,
// minimum of four pixels), `itkRecursiveGaussianImageFilter.hxx` (`SetUp`,
// `ComputeNCoefficients`, `ComputeDCoefficients`, `ComputeRemainingCoefficients`),
// `itkRecursiveSeparableImageFilter.hxx` (`FilterDataArray`), `itkImageRegistrationMethodv4.hxx`
// (`InitializeRegistrationAtEachLevel`: per-level smoothing of the full-resolution images,
// sigma units, shrunk virtual domain) and from ANTs `itkantsRegistrationHelper.hxx`
// (`CalculateShrinkFactorsPerDimension`). See `docs/THIRD_PARTY_PORTS.md`.

//! Multi-resolution pyramid of `antsRegistration`: shrunk virtual domains and the recursive
//! Gaussian smoothing of the full-resolution images.
//!
//! ITK does not build a shrunk image pyramid for the metric images. Per level it
//! (1) shrinks the **virtual domain** (the fixed image grid) with `ShrinkImageFilter` and
//! (2) smooths the **full-resolution** fixed and moving images with
//! `SmoothingRecursiveGaussianImageFilter`; the metric samples those smoothed images through
//! the transforms at the points of the shrunk virtual grid. [`level_virtual_grid`] and
//! [`smooth_for_level`] are those two steps; [`shrink_volume`] is the `ShrinkImageFilter` data
//! path for callers that need image values on the shrunk grid (SyN works on such images).

use std::borrow::Cow;

use crate::error::{invalid, Result};
use crate::grid::Grid;
use crate::linalg::Vec3;
use crate::volume::Volume;
use rayon::prelude::*;

/// Unit of an `antsRegistration` smoothing sigma (`3x2x1x0vox` vs `...mm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigmaUnits {
    /// Voxels: the sigma of an axis is multiplied by that image's spacing along the axis.
    Voxels,
    /// Millimetres, used as given.
    Millimetres,
}

/// ANTs `CalculateShrinkFactorsPerDimension`: the requested factor goes to the axis of smallest
/// spacing (the first one on ties); every other axis gets the factor in `1..=factor` whose
/// resulting spacing is closest to that axis' shrunk spacing (smaller factor on ties). For an
/// isotropic image every axis gets `factor`.
pub fn shrink_factors_per_dimension(factor: u32, spacing: Vec3) -> [u32; 3] {
    let factor = factor.max(1);
    let mut min_index = 0;
    let mut min_spacing = spacing[0];
    for (n, s) in spacing.iter().enumerate().skip(1) {
        if min_spacing > *s {
            min_spacing = *s;
            min_index = n;
        }
    }
    let target = spacing[min_index] * f64::from(factor);
    let mut out = [0u32; 3];
    out[min_index] = factor;
    for n in 0..3 {
        if out[n] != 0 {
            continue;
        }
        let mut best_diff = (spacing[n] * f64::from(factor) - target).abs();
        let mut best = factor;
        for f in (1..factor).rev() {
            let diff = (spacing[n] * f64::from(f) - target).abs();
            if diff <= best_diff {
                best_diff = diff;
                best = f;
            }
        }
        out[n] = best;
    }
    out
}

fn round_half_up(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// ITK `ShrinkImageFilter` output grid: size `max(1, floor(n / f))`, spacing `s * f`, direction
/// unchanged, origin shifted so that the physical centres of input and output coincide.
pub fn shrink_grid(grid: &Grid, factors: [u32; 3]) -> Result<Grid> {
    let size = grid.size();
    let spacing = grid.spacing();
    let mut out_size = [0usize; 3];
    let mut out_spacing = [0.0; 3];
    for a in 0..3 {
        let f = factors[a].max(1);
        out_spacing[a] = spacing[a] * f64::from(f);
        out_size[a] = ((size[a] as f64 / f64::from(f)).floor() as usize).max(1);
    }
    let in_centre = [
        (size[0] as f64 - 1.0) / 2.0,
        (size[1] as f64 - 1.0) / 2.0,
        (size[2] as f64 - 1.0) / 2.0,
    ];
    let out_centre = [
        (out_size[0] as f64 - 1.0) / 2.0,
        (out_size[1] as f64 - 1.0) / 2.0,
        (out_size[2] as f64 - 1.0) / 2.0,
    ];
    let in_centre_point = grid.index_to_point(in_centre);
    let provisional = Grid::new(out_size, out_spacing, grid.origin(), grid.direction())?;
    let out_centre_point = provisional.index_to_point(out_centre);
    let origin = [
        grid.origin()[0] + (in_centre_point[0] - out_centre_point[0]),
        grid.origin()[1] + (in_centre_point[1] - out_centre_point[1]),
        grid.origin()[2] + (in_centre_point[2] - out_centre_point[2]),
    ];
    Grid::new(out_size, out_spacing, origin, grid.direction())
}

/// ITK `ShrinkImageFilter` data path: pure subsampling, `input = output * factor + offset` with
/// `offset` the input index (rounded half up, at least zero) of the output grid's origin.
/// Returns the shrunk grid and the per-axis offsets.
pub fn shrink_offsets(grid: &Grid, factors: [u32; 3]) -> Result<(Grid, [usize; 3])> {
    let out = shrink_grid(grid, factors)?;
    let c = grid.point_to_index(out.origin());
    let size = grid.size();
    let osize = out.size();
    let mut offset = [0usize; 3];
    for a in 0..3 {
        let f = factors[a].max(1) as usize;
        let o = round_half_up(c[a]).max(0) as usize;
        if o + (osize[a] - 1) * f > size[a] - 1 {
            return Err(invalid(
                "shrink",
                format!("axis {a}: subsampling leaves the input (offset {o}, factor {f})"),
            ));
        }
        offset[a] = o;
    }
    Ok((out, offset))
}

/// The virtual domain of a level: the fixed image grid shrunk by `factors`.
pub fn level_virtual_grid(fixed: &Grid, factors: [u32; 3]) -> Result<Grid> {
    shrink_grid(fixed, factors)
}

/// `ShrinkImageFilter` applied to a volume.
pub fn shrink_volume<T: Copy>(vol: &Volume<T>, factors: [u32; 3]) -> Result<Volume<T>> {
    let (out, offset) = shrink_offsets(vol.grid(), factors)?;
    let f = [
        factors[0].max(1) as usize,
        factors[1].max(1) as usize,
        factors[2].max(1) as usize,
    ];
    let [ox, oy, oz] = out.size();
    let mut data = Vec::with_capacity(out.num_voxels());
    for k in 0..oz {
        for j in 0..oy {
            for i in 0..ox {
                let idx = vol.grid().linear_index(
                    i * f[0] + offset[0],
                    j * f[1] + offset[1],
                    k * f[2] + offset[2],
                );
                data.push(vol.data()[idx]);
            }
        }
    }
    Volume::from_vec(out, data)
}

#[derive(Clone, Copy, Debug)]
struct Deriche {
    n: [f64; 4],
    d: [f64; 4],
    m: [f64; 4],
    bn: [f64; 4],
    bm: [f64; 4],
}

impl Deriche {
    fn zero_order(sigma: f64, spacing: f64) -> Result<Self> {
        if spacing < 1e-8 {
            return Err(invalid(
                "smoothing",
                format!("spacing {spacing} is too small"),
            ));
        }
        let sigmad = sigma / spacing;
        let (a1, b1, w1, l1) = (1.3530, 1.8151, 0.6681, -1.3932);
        let (a2, b2, w2, l2) = (-0.3531, 0.0902, 2.0787, -1.3732);
        let sin1 = (w1 / sigmad).sin();
        let sin2 = (w2 / sigmad).sin();
        let cos1 = (w1 / sigmad).cos();
        let cos2 = (w2 / sigmad).cos();
        let exp1 = (l1 / sigmad).exp();
        let exp2 = (l2 / sigmad).exp();

        let d4 = exp1 * exp1 * exp2 * exp2;
        let mut d3 = -2.0 * cos1 * exp1 * exp2 * exp2;
        d3 += -2.0 * cos2 * exp2 * exp1 * exp1;
        let mut d2 = 4.0 * cos2 * cos1 * exp1 * exp2;
        d2 += exp1 * exp1 + exp2 * exp2;
        let d1 = -2.0 * (exp2 * cos2 + exp1 * cos1);
        let sd = 1.0 + d1 + d2 + d3 + d4;

        let mut n0 = a1 + a2;
        let mut n1 = exp2 * (b2 * sin2 - (a2 + 2.0 * a1) * cos2);
        n1 += exp1 * (b1 * sin1 - (a1 + 2.0 * a2) * cos1);
        let mut n2 = (a1 + a2) * cos2 * cos1;
        n2 -= b1 * cos2 * sin1 + b2 * cos1 * sin2;
        n2 *= 2.0 * exp1 * exp2;
        n2 += a2 * exp1 * exp1 + a1 * exp2 * exp2;
        let mut n3 = exp2 * exp1 * exp1 * (b2 * sin2 - a2 * cos2);
        n3 += exp1 * exp2 * exp2 * (b1 * sin1 - a1 * cos1);
        let sn = n0 + n1 + n2 + n3;

        let alpha0 = 2.0 * sn / sd - n0;
        n0 *= 1.0 / alpha0;
        n1 *= 1.0 / alpha0;
        n2 *= 1.0 / alpha0;
        n3 *= 1.0 / alpha0;

        let m1 = n1 - d1 * n0;
        let m2 = n2 - d2 * n0;
        let m3 = n3 - d3 * n0;
        let m4 = -d4 * n0;
        let sn = n0 + n1 + n2 + n3;
        let sm = m1 + m2 + m3 + m4;
        let sd = 1.0 + d1 + d2 + d3 + d4;
        Ok(Self {
            n: [n0, n1, n2, n3],
            d: [d1, d2, d3, d4],
            m: [m1, m2, m3, m4],
            bn: [d1 * sn / sd, d2 * sn / sd, d3 * sn / sd, d4 * sn / sd],
            bm: [d1 * sm / sd, d2 * sm / sd, d3 * sm / sd, d4 * sm / sd],
        })
    }

    fn filter_line(
        &self,
        data: &[f64],
        causal: &mut [f64],
        anticausal: &mut [f64],
        out: &mut [f64],
    ) {
        let ln = data.len();
        let (n, d, m, bn, bm) = (&self.n, &self.d, &self.m, &self.bn, &self.bm);
        let v1 = data[0];
        causal[0] = v1 * n[0] + v1 * n[1] + v1 * n[2] + v1 * n[3];
        causal[1] = data[1] * n[0] + v1 * n[1] + v1 * n[2] + v1 * n[3];
        causal[2] = data[2] * n[0] + data[1] * n[1] + v1 * n[2] + v1 * n[3];
        causal[3] = data[3] * n[0] + data[2] * n[1] + data[1] * n[2] + v1 * n[3];
        causal[0] -= v1 * bn[0] + v1 * bn[1] + v1 * bn[2] + v1 * bn[3];
        causal[1] -= causal[0] * d[0] + v1 * bn[1] + v1 * bn[2] + v1 * bn[3];
        causal[2] -= causal[1] * d[0] + causal[0] * d[1] + v1 * bn[2] + v1 * bn[3];
        causal[3] -= causal[2] * d[0] + causal[1] * d[1] + causal[0] * d[2] + v1 * bn[3];
        for i in 4..ln {
            causal[i] =
                data[i] * n[0] + data[i - 1] * n[1] + data[i - 2] * n[2] + data[i - 3] * n[3];
            causal[i] -= causal[i - 1] * d[0]
                + causal[i - 2] * d[1]
                + causal[i - 3] * d[2]
                + causal[i - 4] * d[3];
        }

        let v2 = data[ln - 1];
        anticausal[ln - 1] = v2 * m[0] + v2 * m[1] + v2 * m[2] + v2 * m[3];
        anticausal[ln - 2] = data[ln - 1] * m[0] + v2 * m[1] + v2 * m[2] + v2 * m[3];
        anticausal[ln - 3] = data[ln - 2] * m[0] + data[ln - 1] * m[1] + v2 * m[2] + v2 * m[3];
        anticausal[ln - 4] =
            data[ln - 3] * m[0] + data[ln - 2] * m[1] + data[ln - 1] * m[2] + v2 * m[3];
        anticausal[ln - 1] -= v2 * bm[0] + v2 * bm[1] + v2 * bm[2] + v2 * bm[3];
        anticausal[ln - 2] -= anticausal[ln - 1] * d[0] + v2 * bm[1] + v2 * bm[2] + v2 * bm[3];
        anticausal[ln - 3] -=
            anticausal[ln - 2] * d[0] + anticausal[ln - 1] * d[1] + v2 * bm[2] + v2 * bm[3];
        anticausal[ln - 4] -= anticausal[ln - 3] * d[0]
            + anticausal[ln - 2] * d[1]
            + anticausal[ln - 1] * d[2]
            + v2 * bm[3];
        for i in (1..=ln - 4).rev() {
            anticausal[i - 1] =
                data[i] * m[0] + data[i + 1] * m[1] + data[i + 2] * m[2] + data[i + 3] * m[3];
            anticausal[i - 1] -= anticausal[i] * d[0]
                + anticausal[i + 1] * d[1]
                + anticausal[i + 2] * d[2]
                + anticausal[i + 3] * d[3];
        }
        for i in 0..ln {
            out[i] = causal[i] + anticausal[i];
        }
    }
}

/// One axis of ITK's `RecursiveGaussianImageFilter` (zero order, no scale normalisation):
/// Deriche's fourth-order recursive approximation with constant edge extension. `sigma_mm` is
/// the sigma in millimetres along `axis`; the filter divides by the spacing of that axis. The
/// axis needs at least four voxels.
#[cfg(test)]
pub fn smooth_axis(vol: &Volume<f64>, axis: usize, sigma_mm: f64) -> Result<Volume<f64>> {
    let coeffs = axis_coefficients(vol, axis, sigma_mm)?;
    let mut out = vol.data().to_vec();
    filter_axis_in_place(&mut out, vol.grid().size(), axis, &coeffs);
    Volume::from_vec(*vol.grid(), out)
}

fn axis_coefficients(vol: &Volume<f64>, axis: usize, sigma_mm: f64) -> Result<Deriche> {
    let size = vol.grid().size();
    if size[axis] < 4 {
        return Err(invalid(
            "smoothing",
            format!(
                "axis {axis} has {} voxels, at least 4 are required",
                size[axis]
            ),
        ));
    }
    if sigma_mm.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err(invalid("smoothing", "sigma must be greater than zero"));
    }
    Deriche::zero_order(sigma_mm, vol.grid().spacing()[axis])
}

struct LineScratch {
    line: Vec<f64>,
    res: Vec<f64>,
    causal: Vec<f64>,
    anticausal: Vec<f64>,
}

impl LineScratch {
    fn new(len: usize) -> Self {
        Self {
            line: vec![0.0; len],
            res: vec![0.0; len],
            causal: vec![0.0; len],
            anticausal: vec![0.0; len],
        }
    }

    fn filter(&mut self, coeffs: &Deriche) {
        coeffs.filter_line(
            &self.line,
            &mut self.causal,
            &mut self.anticausal,
            &mut self.res,
        );
    }
}

/// Filters every line of `data` (x fastest, `size` voxels) along `axis`, lines in parallel; each
/// line is computed exactly as the sequential filter would.
fn filter_axis_in_place(data: &mut [f64], size: [usize; 3], axis: usize, coeffs: &Deriche) {
    let [nx, ny, nz] = size;
    let ln = size[axis];
    match axis {
        0 => data.par_chunks_mut(nx).for_each_init(
            || LineScratch::new(ln),
            |sc, chunk| {
                sc.line.copy_from_slice(chunk);
                sc.filter(coeffs);
                chunk.copy_from_slice(&sc.res);
            },
        ),
        1 => data.par_chunks_mut(nx * ny).for_each_init(
            || LineScratch::new(ln),
            |sc, slab| {
                for i in 0..nx {
                    for (t, l) in sc.line.iter_mut().enumerate() {
                        *l = slab[i + nx * t];
                    }
                    sc.filter(coeffs);
                    for (t, r) in sc.res.iter().enumerate() {
                        slab[i + nx * t] = *r;
                    }
                }
            },
        ),
        _ => {
            let src: &[f64] = data;
            let blocks: Vec<Vec<f64>> = (0..ny)
                .into_par_iter()
                .map_init(
                    || LineScratch::new(ln),
                    |sc, j| {
                        let mut block = vec![0.0; nx * nz];
                        for t in 0..nz {
                            let row = (t * ny + j) * nx;
                            block[t * nx..(t + 1) * nx].copy_from_slice(&src[row..row + nx]);
                        }
                        for i in 0..nx {
                            for (t, l) in sc.line.iter_mut().enumerate() {
                                *l = block[i + nx * t];
                            }
                            sc.filter(coeffs);
                            for (t, r) in sc.res.iter().enumerate() {
                                block[i + nx * t] = *r;
                            }
                        }
                        block
                    },
                )
                .collect();
            data.par_chunks_mut(nx * ny)
                .enumerate()
                .for_each(|(t, slab)| {
                    for (j, block) in blocks.iter().enumerate() {
                        slab[j * nx..(j + 1) * nx].copy_from_slice(&block[t * nx..(t + 1) * nx]);
                    }
                });
        }
    }
}

/// ITK `SmoothingRecursiveGaussianImageFilter` with a per-axis sigma in millimetres: the axes
/// are filtered in the order z, x, y.
pub fn smooth_recursive_gaussian(vol: &Volume<f64>, sigma_mm: Vec3) -> Result<Volume<f64>> {
    let size = vol.grid().size();
    let mut out = vol.data().to_vec();
    for axis in [2usize, 0, 1] {
        let coeffs = axis_coefficients(vol, axis, sigma_mm[axis])?;
        filter_axis_in_place(&mut out, size, axis, &coeffs);
    }
    Volume::from_vec(*vol.grid(), out)
}

/// The per-axis sigma in millimetres of a level: `sigma * spacing` for [`SigmaUnits::Voxels`]
/// (the spacing of the image being smoothed), `sigma` for [`SigmaUnits::Millimetres`].
pub fn sigma_mm_per_axis(sigma: f64, units: SigmaUnits, spacing: Vec3) -> Vec3 {
    match units {
        SigmaUnits::Voxels => [sigma * spacing[0], sigma * spacing[1], sigma * spacing[2]],
        SigmaUnits::Millimetres => [sigma; 3],
    }
}

/// The smoothed full-resolution image of a level, as `ImageRegistrationMethodv4` computes it:
/// a copy for `sigma <= 0`, otherwise [`smooth_recursive_gaussian`] with
/// [`sigma_mm_per_axis`] (the spacing of `vol` itself).
pub fn smooth_for_level(vol: &Volume<f64>, sigma: f64, units: SigmaUnits) -> Result<Volume<f64>> {
    smooth_for_level_cow(vol, sigma, units).map(Cow::into_owned)
}

pub fn smooth_for_level_cow(
    vol: &Volume<f64>,
    sigma: f64,
    units: SigmaUnits,
) -> Result<Cow<'_, Volume<f64>>> {
    if sigma <= 0.0 {
        return Ok(Cow::Borrowed(vol));
    }
    smooth_recursive_gaussian(vol, sigma_mm_per_axis(sigma, units, vol.grid().spacing()))
        .map(Cow::Owned)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    fn grid(size: [usize; 3], spacing: Vec3, origin: Vec3) -> Grid {
        Grid::new(size, spacing, origin, IDENTITY3).unwrap()
    }

    #[test]
    fn shrink_grid_follows_the_itk_centre_rule() {
        let g = grid([197, 233, 189], [1.0; 3], [-98.0, -126.0, -72.0]);
        let s = shrink_grid(&g, [8, 8, 8]).unwrap();
        assert_eq!(s.size(), [24, 29, 23]);
        assert_eq!(s.spacing(), [8.0; 3]);
        let o = s.origin();
        assert!((o[0] - (-98.0 + (98.0 - 11.5 * 8.0))).abs() < 1e-12);
        assert!((o[1] - (-126.0 + (116.0 - 14.0 * 8.0))).abs() < 1e-12);
        assert!((o[2] - (-72.0 + (94.0 - 11.0 * 8.0))).abs() < 1e-12);
        let ci = g.index_to_point([98.0, 116.0, 94.0]);
        let co = s.index_to_point([11.5, 14.0, 11.0]);
        for a in 0..3 {
            assert!((ci[a] - co[a]).abs() < 1e-12);
        }
        let (_, off) = shrink_offsets(&g, [8, 8, 8]).unwrap();
        assert_eq!(off, [6, 4, 6]);
    }

    #[test]
    fn shrink_by_two_of_an_even_axis_rounds_the_half_voxel_offset_up() {
        let g = grid([10, 10, 10], [1.5; 3], [3.0, 4.0, 5.0]);
        let s = shrink_grid(&g, [2, 2, 2]).unwrap();
        assert_eq!(s.size(), [5, 5, 5]);
        assert!((s.origin()[0] - (3.0 + 0.5 * 1.5)).abs() < 1e-12);
        let (_, off) = shrink_offsets(&g, [2, 2, 2]).unwrap();
        assert_eq!(off, [1, 1, 1]);
        let data: Vec<f64> = (0..1000).map(f64::from).collect();
        let v = Volume::from_vec(g, data).unwrap();
        let sv = shrink_volume(&v, [2, 2, 2]).unwrap();
        assert_eq!(sv.get(0, 0, 0), Some(111.0));
        assert_eq!(sv.get(1, 0, 0), Some(113.0));
        assert_eq!(sv.get(0, 1, 0), Some(131.0));
        assert_eq!(sv.get(0, 0, 1), Some(311.0));
    }

    #[test]
    fn factor_one_is_the_identity_and_big_factors_collapse_to_one_voxel() {
        let g = grid([7, 6, 5], [1.0, 2.0, 3.0], [1.0, 2.0, 3.0]);
        let s = shrink_grid(&g, [1, 1, 1]).unwrap();
        assert!(s.approx_eq(&g, 1e-12));
        let t = shrink_grid(&g, [100, 100, 100]).unwrap();
        assert_eq!(t.size(), [1, 1, 1]);
    }

    #[test]
    fn per_dimension_factors_follow_the_ants_rule() {
        assert_eq!(shrink_factors_per_dimension(8, [1.0; 3]), [8, 8, 8]);
        assert_eq!(shrink_factors_per_dimension(4, [1.0, 1.0, 2.0]), [4, 4, 2]);
        assert_eq!(shrink_factors_per_dimension(4, [0.5, 0.5, 3.0]), [4, 4, 1]);
        assert_eq!(shrink_factors_per_dimension(8, [2.0, 1.0, 1.0]), [4, 8, 8]);
        assert_eq!(shrink_factors_per_dimension(1, [0.5, 1.0, 3.0]), [1, 1, 1]);
    }

    #[test]
    fn sigma_units() {
        assert_eq!(
            sigma_mm_per_axis(2.0, SigmaUnits::Voxels, [0.5, 1.0, 3.0]),
            [1.0, 2.0, 6.0]
        );
        assert_eq!(
            sigma_mm_per_axis(2.0, SigmaUnits::Millimetres, [0.5, 1.0, 3.0]),
            [2.0; 3]
        );
    }

    #[test]
    fn recursive_gaussian_keeps_constants_and_has_unit_dc_gain_and_the_requested_variance() {
        let g = grid([4, 4, 4], [1.0; 3], [0.0; 3]);
        let c = Volume::from_vec(g, vec![7.0; 64]).unwrap();
        let s = smooth_recursive_gaussian(&c, [1.5, 1.5, 1.5]).unwrap();
        assert!(s.data().iter().all(|v| (v - 7.0).abs() < 1e-9));

        let g = grid([81, 4, 4], [2.0, 1.0, 1.0], [0.0; 3]);
        let mut d = vec![0.0; g.num_voxels()];
        d[g.linear_index(40, 1, 1)] = 1.0;
        let v = Volume::from_vec(g, d).unwrap();
        let sigma_vox = 3.0;
        let s = smooth_axis(&v, 0, sigma_vox * 2.0).unwrap();
        let line: Vec<f64> = (0..81).map(|i| s.get(i, 1, 1).unwrap()).collect();
        let sum: f64 = line.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6, "{sum}");
        let var: f64 = line
            .iter()
            .enumerate()
            .map(|(i, w)| w * (i as f64 - 40.0).powi(2))
            .sum();
        assert!(
            (var - sigma_vox * sigma_vox).abs() < 0.1 * sigma_vox * sigma_vox,
            "{var}"
        );
        let peak = 1.0 / (sigma_vox * (2.0 * std::f64::consts::PI).sqrt());
        let worst = (0..81)
            .map(|i| {
                let x = i as f64 - 40.0;
                (line[i] - peak * (-x * x / (2.0 * sigma_vox * sigma_vox)).exp()).abs()
            })
            .fold(0.0, f64::max);
        assert!(
            worst < 0.01 * peak,
            "worst deviation {worst} of peak {peak}"
        );
        for i in 0..40 {
            assert!((line[40 - i] - line[40 + i]).abs() < 1e-6 * peak + 1e-12);
        }
    }

    #[test]
    fn smoothing_rejects_short_axes_and_zero_sigma_is_a_copy() {
        let g = grid([3, 8, 8], [1.0; 3], [0.0; 3]);
        let v = Volume::from_vec(g, vec![1.0; g.num_voxels()]).unwrap();
        assert!(smooth_axis(&v, 0, 1.0).is_err());
        assert!(smooth_axis(&v, 1, 0.0).is_err());
        let copy = smooth_for_level(&v, 0.0, SigmaUnits::Voxels).unwrap();
        assert_eq!(copy, v);
        let borrowed = smooth_for_level_cow(&v, 0.0, SigmaUnits::Voxels).unwrap();
        assert!(matches!(borrowed, Cow::Borrowed(b) if std::ptr::eq(b, &v)));
    }

    #[test]
    fn smoothing_reduces_noise_energy_and_commutes_with_axis_order_up_to_rounding() {
        let g = grid([12, 12, 12], [1.0; 3], [0.0; 3]);
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|i| ((i * 7919) % 97) as f64 / 97.0)
            .collect();
        let v = Volume::from_vec(g, data).unwrap();
        let s = smooth_for_level(&v, 1.0, SigmaUnits::Voxels).unwrap();
        match smooth_for_level_cow(&v, 1.0, SigmaUnits::Voxels).unwrap() {
            Cow::Owned(o) => assert_eq!(o, s),
            Cow::Borrowed(_) => panic!("a positive sigma must smooth"),
        }
        let var = |x: &[f64]| {
            let m = x.iter().sum::<f64>() / x.len() as f64;
            x.iter().map(|a| (a - m).powi(2)).sum::<f64>() / x.len() as f64
        };
        assert!(var(s.data()) < 0.2 * var(v.data()));
        let alt = smooth_axis(
            &smooth_axis(&smooth_axis(&v, 1, 1.0).unwrap(), 0, 1.0).unwrap(),
            2,
            1.0,
        )
        .unwrap();
        for (a, b) in s.data().iter().zip(alt.data()) {
            assert!((a - b).abs() < 1e-10);
        }
    }
}
