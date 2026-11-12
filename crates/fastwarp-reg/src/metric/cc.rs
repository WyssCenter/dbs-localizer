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
// Portions derived from the ANTs / ITK sources
// `itkANTSNeighborhoodCorrelationImageToImageMetricv4GetValueAndDerivativeThreader.hxx`
// (`ComputeInformationFromQueues`, `ComputeMovingTransformDerivative`: window sums over the valid
// points of the neighbourhood, centred second moments, `localCC`, the derivative at the centre
// voxel), from `itkImageToImageMetricv4.hxx` (`TransformAndEvaluateFixedPoint`,
// `TransformAndEvaluateMovingPoint`, `ComputeMovingImageGradientAtPoint`) and from
// `itkImageToImageMetricv4GetValueAndDerivativeThreaderBase.hxx` (`AfterThreadedExecution`: the
// value is the mean over valid points, a local-support derivative is not normalised). See
// `docs/THIRD_PARTY_PORTS.md`.

//! ANTs neighbourhood cross-correlation (`CC[fixed,moving,weight,radius]`), dense CPU oracle.
//!
//! **Sign convention.** As in ITK, the metric value is minimised: `value = -mean(localCC)`, and
//! the returned derivative is the direction in which the parameters (or the displacement) are
//! *added* to decrease the value, i.e. `derivative = -d value / d theta` (before ITK's metric
//! weight). Finite-difference checks therefore compare against `-(v(+h) - v(-h)) / 2h`.

use rayon::prelude::*;

use crate::error::{invalid, Result};
use crate::grid::Grid;
use crate::interp::{central_difference_at_point, linear_at_point};
use crate::linalg::Vec3;
use crate::transform::TransformChain;
use crate::volume::Volume;

/// The neighbourhood radius in voxels (ANTs `CC[...,radius]` sets all three axes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CcMetric {
    /// Radius per axis; the window has `2 r + 1` voxels per axis.
    pub radius: [usize; 3],
}

impl CcMetric {
    /// The same radius on every axis.
    pub fn new(radius: usize) -> Self {
        Self {
            radius: [radius; 3],
        }
    }
}

/// What the metric needs: the virtual domain (the level grid), the two images and the
/// transforms from virtual-domain points to the fixed and the moving image
/// (`TransformChain::apply`, last element first). For SyN the images are already warped onto
/// the virtual grid and both chains are empty.
#[derive(Clone, Copy)]
pub struct CcInputs<'a> {
    /// The virtual domain.
    pub virtual_grid: &'a Grid,
    /// The fixed image.
    pub fixed: &'a Volume<f64>,
    /// The moving image.
    pub moving: &'a Volume<f64>,
    /// Virtual point to fixed image point.
    pub fixed_transform: &'a TransformChain,
    /// Virtual point to moving image point.
    pub moving_transform: &'a TransformChain,
}

/// Result of one evaluation; all per-voxel vectors are indexed like the virtual grid.
#[derive(Clone, Debug)]
pub struct CcEvaluation {
    /// `-mean(localCC)` over the valid voxels.
    pub value: f64,
    /// Number of valid voxels (both mapped points inside their image buffers).
    pub num_valid: usize,
    /// Whether the voxel is valid.
    pub valid: Vec<bool>,
    /// `localCC` per voxel (zero for invalid voxels); `1` where the window is degenerate.
    pub local_cc: Vec<f64>,
    /// The scalar `2 sFM / (sFF sMM) * ((F - Fmean) - sFM / sMM * (M - Mmean))`, zero when
    /// `sFF` or `sMM` is not above machine epsilon or when derivatives were not requested.
    pub coefficient: Vec<f64>,
    /// `coefficient * grad M` per voxel: the image-space derivative of a dense transform (ITK
    /// leaves it undivided by the number of valid points). Empty when not requested.
    pub derivative: Vec<Vec3>,
}

#[cfg(test)]
impl CcEvaluation {
    /// The parameter derivative for a global transform: `sum_v derivative[v] . J_k(p_v) / N`.
    /// `jacobian(p)` returns `dT/dtheta_k` for every parameter at the virtual point `p`.
    pub fn parameter_derivative(
        &self,
        grid: &Grid,
        number_of_parameters: usize,
        jacobian: &dyn Fn(Vec3) -> Vec<Vec3>,
    ) -> Vec<f64> {
        let mut out = vec![0.0; number_of_parameters];
        if self.derivative.is_empty() || self.num_valid == 0 {
            return out;
        }
        for (idx, d) in self.derivative.iter().enumerate() {
            if !self.valid[idx] {
                continue;
            }
            let [i, j, k] = grid.ijk(idx);
            let p = grid.index_to_point([i as f64, j as f64, k as f64]);
            let cols = jacobian(p);
            for (o, c) in out.iter_mut().zip(&cols) {
                *o += d[0] * c[0] + d[1] * c[1] + d[2] * c[2];
            }
        }
        for o in &mut out {
            *o /= self.num_valid as f64;
        }
        out
    }
}

fn box_sum_axis(data: &[[f64; 6]], size: [usize; 3], axis: usize, radius: usize) -> Vec<[f64; 6]> {
    let stride = [1, size[0], size[0] * size[1]][axis];
    let n = size[axis];
    (0..data.len())
        .into_par_iter()
        .map(|idx| {
            let pos = (idx / stride) % n;
            let lo = pos.saturating_sub(radius);
            let hi = (pos + radius).min(n - 1);
            let base = idx - pos * stride;
            let mut acc = [0.0; 6];
            for t in lo..=hi {
                let v = &data[base + t * stride];
                for c in 0..6 {
                    acc[c] += v[c];
                }
            }
            acc
        })
        .collect()
}

impl CcMetric {
    /// Evaluates the metric on the virtual grid. A voxel is valid when its mapped fixed point
    /// and its mapped moving point lie inside the respective image buffers; invalid voxels
    /// contribute nothing to any window sum. Fails when no voxel is valid.
    pub fn evaluate(&self, inp: &CcInputs<'_>, want_derivative: bool) -> Result<CcEvaluation> {
        let grid = inp.virtual_grid;
        let size = grid.size();
        let n = grid.num_voxels();
        let samples: Vec<Option<(f64, f64, Vec3)>> = (0..n)
            .into_par_iter()
            .map(|idx| {
                let [i, j, k] = grid.ijk(idx);
                let p = grid.index_to_point([i as f64, j as f64, k as f64]);
                let fp = inp.fixed_transform.apply(p);
                let fv = linear_at_point(inp.fixed, fp)?;
                let mp = inp.moving_transform.apply(p);
                let mv = linear_at_point(inp.moving, mp)?;
                let g = if want_derivative {
                    central_difference_at_point(inp.moving, mp)
                } else {
                    [0.0; 3]
                };
                Some((fv, mv, g))
            })
            .collect();
        let mut valid = vec![false; n];
        let mut f = vec![0.0; n];
        let mut m = vec![0.0; n];
        let mut grad = if want_derivative {
            vec![[0.0; 3]; n]
        } else {
            Vec::new()
        };
        let mut num_valid = 0usize;
        for (idx, s) in samples.iter().enumerate() {
            if let Some((fv, mv, g)) = s {
                valid[idx] = true;
                f[idx] = *fv;
                m[idx] = *mv;
                if want_derivative {
                    grad[idx] = *g;
                }
                num_valid += 1;
            }
        }
        if num_valid == 0 {
            return Err(invalid(
                "cc metric",
                "no virtual voxel maps inside both image buffers",
            ));
        }

        let mut sums: Vec<[f64; 6]> = (0..n)
            .map(|idx| {
                if valid[idx] {
                    [
                        f[idx] * f[idx],
                        m[idx] * m[idx],
                        f[idx],
                        m[idx],
                        f[idx] * m[idx],
                        1.0,
                    ]
                } else {
                    [0.0; 6]
                }
            })
            .collect();
        for axis in 0..3 {
            sums = box_sum_axis(&sums, size, axis, self.radius[axis]);
        }

        let eps = f64::EPSILON;
        let per_voxel: Vec<(f64, f64, Vec3)> = (0..n)
            .into_par_iter()
            .map(|idx| {
                if !valid[idx] {
                    return (0.0, 0.0, [0.0; 3]);
                }
                let [sum_f2, sum_m2, sum_f, sum_m, sum_fm, count] = sums[idx];
                let fixed_mean = sum_f / count;
                let moving_mean = sum_m / count;
                let s_ff = sum_f2 - fixed_mean * sum_f - fixed_mean * sum_f
                    + count * fixed_mean * fixed_mean;
                let s_mm = sum_m2 - moving_mean * sum_m - moving_mean * sum_m
                    + count * moving_mean * moving_mean;
                let s_fm = sum_fm - moving_mean * sum_f - fixed_mean * sum_m
                    + count * moving_mean * fixed_mean;
                let fixed_i = f[idx] - fixed_mean;
                let moving_i = m[idx] - moving_mean;
                let product = s_ff * s_mm;
                let mut cc = 1.0;
                if product.abs() > eps {
                    cc = s_fm * s_fm / product;
                }
                if want_derivative && s_ff > eps && s_mm > eps {
                    let coeff = 2.0 * s_fm / product * (fixed_i - s_fm / s_mm * moving_i);
                    (
                        cc,
                        coeff,
                        [
                            coeff * grad[idx][0],
                            coeff * grad[idx][1],
                            coeff * grad[idx][2],
                        ],
                    )
                } else {
                    (cc, 0.0, [0.0; 3])
                }
            })
            .collect();
        let mut local_cc = vec![0.0; n];
        let mut coefficient = vec![0.0; n];
        let mut derivative = if want_derivative {
            vec![[0.0; 3]; n]
        } else {
            Vec::new()
        };
        let mut total = 0.0;
        for (idx, (cc, coeff, d)) in per_voxel.into_iter().enumerate() {
            if !valid[idx] {
                continue;
            }
            local_cc[idx] = cc;
            total -= cc;
            coefficient[idx] = coeff;
            if want_derivative {
                derivative[idx] = d;
            }
        }
        Ok(CcEvaluation {
            value: total / num_valid as f64,
            num_valid,
            valid,
            local_cc,
            coefficient,
            derivative,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_range_loop)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;
    use crate::transform::{AffineTransform, TransformElem};

    fn grid(n: usize) -> Grid {
        Grid::new([n, n, n], [1.0; 3], [0.0; 3], IDENTITY3).unwrap()
    }

    fn noise(g: &Grid, seed: usize) -> Volume<f64> {
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|i| (((i + seed) * 2_654_435_761usize) % 1000) as f64 / 1000.0)
            .collect();
        Volume::from_vec(*g, data).unwrap()
    }

    fn smooth(g: &Grid, shift: Vec3) -> Volume<f64> {
        let mut data = vec![0.0; g.num_voxels()];
        for idx in 0..g.num_voxels() {
            let [i, j, k] = g.ijk(idx);
            let (x, y, z) = (
                i as f64 - shift[0],
                j as f64 - shift[1],
                k as f64 - shift[2],
            );
            data[idx] = 1.0
                + (0.45 * x).sin() * (0.35 * y).cos()
                + 0.5 * (0.3 * z + 0.2 * x).sin()
                + 0.002 * (x * y);
        }
        Volume::from_vec(*g, data).unwrap()
    }

    fn translation(t: Vec3) -> TransformChain {
        TransformChain::from_queue(vec![TransformElem::Affine(
            AffineTransform::from_translation(t),
        )])
    }

    fn brute(
        g: &Grid,
        f: &Volume<f64>,
        m: &Volume<f64>,
        valid: &[bool],
        r: usize,
        idx: usize,
    ) -> f64 {
        let [nx, ny, nz] = g.size();
        let [ci, cj, ck] = g.ijk(idx);
        let (mut sf, mut sm, mut sff, mut smm, mut sfm, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for k in ck.saturating_sub(r)..=(ck + r).min(nz - 1) {
            for j in cj.saturating_sub(r)..=(cj + r).min(ny - 1) {
                for i in ci.saturating_sub(r)..=(ci + r).min(nx - 1) {
                    let l = g.linear_index(i, j, k);
                    if !valid[l] {
                        continue;
                    }
                    let (a, b) = (f.data()[l], m.data()[l]);
                    sf += a;
                    sm += b;
                    sff += a * a;
                    smm += b * b;
                    sfm += a * b;
                    n += 1.0;
                }
            }
        }
        let ffv = sff - sf * sf / n;
        let mmv = smm - sm * sm / n;
        let fmv = sfm - sf * sm / n;
        if (ffv * mmv).abs() > f64::EPSILON {
            fmv * fmv / (ffv * mmv)
        } else {
            1.0
        }
    }

    #[test]
    fn identical_images_have_value_minus_one_and_zero_derivative() {
        let g = grid(10);
        let v = noise(&g, 1);
        let id = TransformChain::new();
        let e = CcMetric::new(4)
            .evaluate(
                &CcInputs {
                    virtual_grid: &g,
                    fixed: &v,
                    moving: &v,
                    fixed_transform: &id,
                    moving_transform: &id,
                },
                true,
            )
            .unwrap();
        assert_eq!(e.num_valid, 1000);
        assert!((e.value + 1.0).abs() < 1e-12);
        assert!(e
            .derivative
            .iter()
            .all(|d| d.iter().all(|c| c.abs() < 1e-9)));
    }

    #[test]
    fn window_statistics_match_a_brute_force_neighbourhood_with_invalid_voxels() {
        let g = grid(9);
        let f = noise(&g, 3);
        let m = noise(&g, 11);
        let id = TransformChain::new();
        let shifted = translation([2.3, -1.2, 0.4]);
        let inp = CcInputs {
            virtual_grid: &g,
            fixed: &f,
            moving: &m,
            fixed_transform: &id,
            moving_transform: &shifted,
        };
        let metric = CcMetric::new(2);
        let e = metric.evaluate(&inp, false).unwrap();
        assert!(e.num_valid < 729 && e.num_valid > 100);
        let mut warped = vec![0.0; g.num_voxels()];
        for idx in 0..g.num_voxels() {
            if e.valid[idx] {
                let [i, j, k] = g.ijk(idx);
                let p = shifted.apply(g.index_to_point([i as f64, j as f64, k as f64]));
                warped[idx] = linear_at_point(&m, p).unwrap();
            }
        }
        let wv = Volume::from_vec(g, warped).unwrap();
        let mut sum = 0.0;
        for idx in 0..g.num_voxels() {
            if e.valid[idx] {
                let want = brute(&g, &f, &wv, &e.valid, 2, idx);
                assert!((e.local_cc[idx] - want).abs() < 1e-9, "voxel {idx}");
                sum += want;
            }
        }
        assert!((e.value + sum / e.num_valid as f64).abs() < 1e-9);
    }

    #[test]
    fn invariant_to_affine_intensity_changes_of_either_image_including_the_derivative() {
        let g = grid(10);
        let f = noise(&g, 5);
        let m = smooth(&g, [0.3, 0.2, 0.1]);
        let m2 = Volume::from_vec(g, m.data().iter().map(|v| -2.5 * v + 7.0).collect()).unwrap();
        let f2 = Volume::from_vec(g, f.data().iter().map(|v| 4.0 * v - 1.0).collect()).unwrap();
        let id = TransformChain::new();
        let run = |a: &Volume<f64>, b: &Volume<f64>| {
            CcMetric::new(3)
                .evaluate(
                    &CcInputs {
                        virtual_grid: &g,
                        fixed: a,
                        moving: b,
                        fixed_transform: &id,
                        moving_transform: &id,
                    },
                    true,
                )
                .unwrap()
        };
        let base = run(&f, &m);
        let a = run(&f, &m2);
        let b = run(&f2, &m);
        for other in [&a, &b] {
            assert!((base.value - other.value).abs() < 1e-9);
            for (x, y) in base.local_cc.iter().zip(&other.local_cc) {
                assert!((x - y).abs() < 1e-9);
            }
        }
        for (x, y) in base.derivative.iter().zip(&a.derivative) {
            for c in 0..3 {
                assert!((x[c] - y[c]).abs() < 1e-8, "{x:?} {y:?}");
            }
        }
    }

    #[test]
    fn coefficient_is_the_derivative_of_the_local_cc_with_respect_to_the_centre_voxel() {
        let g = grid(9);
        let f = noise(&g, 2);
        let m = noise(&g, 9);
        let id = TransformChain::new();
        let metric = CcMetric::new(2);
        let run = |mv: &Volume<f64>| {
            metric
                .evaluate(
                    &CcInputs {
                        virtual_grid: &g,
                        fixed: &f,
                        moving: mv,
                        fixed_transform: &id,
                        moving_transform: &id,
                    },
                    true,
                )
                .unwrap()
        };
        let base = run(&m);
        for [i, j, k] in [[4usize, 4, 4], [0, 0, 0], [8, 3, 5], [1, 7, 2]] {
            let idx = g.linear_index(i, j, k);
            let h = 1e-6;
            let mut up = m.data().to_vec();
            up[idx] += h;
            let mut dn = m.data().to_vec();
            dn[idx] -= h;
            let up = run(&Volume::from_vec(g, up).unwrap());
            let dn = run(&Volume::from_vec(g, dn).unwrap());
            let fd = (up.local_cc[idx] - dn.local_cc[idx]) / (2.0 * h);
            assert!(
                (fd - base.coefficient[idx]).abs() < 1e-5 * (1.0 + fd.abs()),
                "voxel {i},{j},{k}: {fd} vs {}",
                base.coefficient[idx]
            );
        }
    }

    #[test]
    fn stepping_along_the_derivative_decreases_the_value_and_agrees_with_finite_differences() {
        let g = grid(20);
        let f = smooth(&g, [0.0; 3]);
        let m = smooth(&g, [1.3, -0.9, 0.7]);
        let id = TransformChain::new();
        let metric = CcMetric::new(3);
        let value_at = |t: Vec3| {
            metric
                .evaluate(
                    &CcInputs {
                        virtual_grid: &g,
                        fixed: &f,
                        moving: &m,
                        fixed_transform: &id,
                        moving_transform: &translation(t),
                    },
                    false,
                )
                .unwrap()
                .value
        };
        let t0 = [0.31, 0.27, -0.23];
        let base = metric
            .evaluate(
                &CcInputs {
                    virtual_grid: &g,
                    fixed: &f,
                    moving: &m,
                    fixed_transform: &id,
                    moving_transform: &translation(t0),
                },
                true,
            )
            .unwrap();
        let jac = |_p: Vec3| vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let d = base.parameter_derivative(&g, 3, &jac);
        let norm = d.iter().map(|v| v * v).sum::<f64>().sqrt();
        assert!(norm > 1e-4);
        let h = 1e-3;
        let mut fd = [0.0; 3];
        for a in 0..3 {
            let mut up = t0;
            up[a] += h;
            let mut dn = t0;
            dn[a] -= h;
            fd[a] = -(value_at(up) - value_at(dn)) / (2.0 * h);
        }
        let dot: f64 = (0..3).map(|a| d[a] * fd[a]).sum();
        let fd_norm = fd.iter().map(|v| v * v).sum::<f64>().sqrt();
        assert!(dot / (norm * fd_norm) > 0.9, "{d:?} vs {fd:?}");
        let step = 0.05 / norm;
        let moved = [
            t0[0] + step * d[0],
            t0[1] + step * d[1],
            t0[2] + step * d[2],
        ];
        assert!(value_at(moved) < base.value);
    }

    #[test]
    fn no_overlap_is_an_error() {
        let g = grid(6);
        let v = noise(&g, 1);
        let id = TransformChain::new();
        let far = translation([100.0, 0.0, 0.0]);
        let r = CcMetric::new(2).evaluate(
            &CcInputs {
                virtual_grid: &g,
                fixed: &v,
                moving: &v,
                fixed_transform: &id,
                moving_transform: &far,
            },
            false,
        );
        assert!(r.is_err());
    }
}
