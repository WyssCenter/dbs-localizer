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
// Portions derived from ITK `itkMattesMutualInformationImageToImageMetricv4.hxx`
// (`Initialize`: intensity ranges over the sampled points and the moving image, padding, bin
// sizes and normalised minima; `ComputeSingleFixedImageParzenWindowIndex`;
// `GetValueCommonAfterThreadedExecution`; `ComputeResults`),
// `itkMattesMutualInformationImageToImageMetricv4GetValueAndDerivativeThreader.hxx`
// (`ProcessPoint`: cubic B-spline Parzen window on the moving axis, box window on the fixed
// axis, joint PDF and derivative accumulation, `AfterThreadedExecution`: `nFactor`),
// `itkBSplineKernelFunction.h` and `itkBSplineDerivativeKernelFunction.h` (order 3),
// `itkImageToImageMetricv4GetValueAndDerivativeThreaderBase.hxx` (`ProcessVirtualPoint`) and
// `itkCompensatedSummation.h` (Kahan summation of the joint PDF). See
// `docs/THIRD_PARTY_PORTS.md`.

//! Mattes mutual information (`MI[fixed,moving,weight,bins,sampling,percentage]`), CPU oracle,
//! for linear transforms on a sampled point set.
//!
//! **Sign convention.** `value = -MI` (ITK minimises). `derivative[k]` is ITK's metric derivative
//! for a global transform: `+ d MI / d theta_k`, the direction in which the parameters are
//! *added* to decrease the value, so `derivative = -d value / d theta`.

use rayon::prelude::*;

use crate::error::{invalid, Result};
use crate::interp::{central_difference_at_point, linear_at_point};
use crate::linalg::Vec3;
use crate::transform::{LinearMovingTransform, TransformChain};
use crate::volume::Volume;

const PADDING: usize = 2;

/// ITK `BSplineKernelFunction<3>::Evaluate`.
pub fn cubic_bspline(u: f64) -> f64 {
    let a = u.abs();
    if a < 1.0 {
        let s = a * a;
        (4.0 - 6.0 * s + 3.0 * s * a) / 6.0
    } else if a < 2.0 {
        let s = a * a;
        (8.0 - 12.0 * a + 6.0 * s - s * a) / 6.0
    } else {
        0.0
    }
}

/// ITK `BSplineDerivativeKernelFunction<3>::Evaluate`.
pub fn cubic_bspline_derivative(u: f64) -> f64 {
    if (0.0..1.0).contains(&u) {
        -2.0 * u + 1.5 * u * u
    } else if u > -1.0 && u < 0.0 {
        -2.0 * u - 1.5 * u * u
    } else if (1.0..2.0).contains(&u) {
        -2.0 + 2.0 * u - 0.5 * u * u
    } else if u > -2.0 && u <= -1.0 {
        2.0 + 2.0 * u + 0.5 * u * u
    } else {
        0.0
    }
}

struct Kahan {
    sum: f64,
    compensation: f64,
}

impl Kahan {
    fn new() -> Self {
        Self {
            sum: 0.0,
            compensation: 0.0,
        }
    }

    fn add(&mut self, x: f64) {
        let y = x - self.compensation;
        let t = self.sum + y;
        self.compensation = (t - self.sum) - y;
        self.sum = t;
    }
}

/// The Mattes metric configuration: the number of histogram bins per axis (ANTs `MI[...,32,...]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MattesMi {
    /// Histogram bins per axis, including the two padding bins on each side.
    pub bins: usize,
}

/// Intensity ranges and bin geometry, fixed for a stage level (ITK computes them in
/// `Initialize`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MiBinning {
    /// Histogram bins per axis.
    pub bins: usize,
    /// Fixed intensity minimum over the sampled points (nearest voxel).
    pub fixed_min: f64,
    /// Fixed intensity maximum over the sampled points (nearest voxel).
    pub fixed_max: f64,
    /// Moving intensity minimum over the whole moving image.
    pub moving_min: f64,
    /// Moving intensity maximum over the whole moving image.
    pub moving_max: f64,
    /// `(fixed_max - fixed_min) / (bins - 4)`.
    pub fixed_bin_size: f64,
    /// `(moving_max - moving_min) / (bins - 4)`.
    pub moving_bin_size: f64,
    /// `fixed_min / fixed_bin_size - 2`.
    pub fixed_normalized_min: f64,
    /// `moving_min / moving_bin_size - 2`.
    pub moving_normalized_min: f64,
}

fn nearest_index_value(vol: &Volume<f64>, p: Vec3) -> Option<f64> {
    let c = vol.grid().point_to_index(p);
    let size = vol.grid().size();
    let mut idx = [0usize; 3];
    for a in 0..3 {
        let r = (c[a] + 0.5).floor();
        if r < 0.0 || r >= size[a] as f64 {
            return None;
        }
        idx[a] = r as usize;
    }
    Some(vol.data()[vol.grid().linear_index(idx[0], idx[1], idx[2])])
}

impl MiBinning {
    /// ITK's `Initialize`: the fixed range from the sample points (mapped by `fixed_transform`
    /// into the fixed image, nearest voxel, points outside skipped), the moving range from the
    /// whole moving image. Fails for no usable sample, a constant image, or `bins <= 4`.
    pub fn new(
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        samples: &[Vec3],
        fixed_transform: &TransformChain,
        bins: usize,
    ) -> Result<Self> {
        if bins <= 2 * PADDING {
            return Err(invalid(
                "mattes",
                "at least five histogram bins are required",
            ));
        }
        let observed: Vec<Option<f64>> = samples
            .par_iter()
            .map(|s| nearest_index_value(fixed, fixed_transform.apply(*s)))
            .collect();
        let mut fixed_min = f64::MAX;
        let mut fixed_max = f64::MIN;
        for v in observed.into_iter().flatten() {
            if v.is_finite() {
                fixed_min = fixed_min.min(v);
                fixed_max = fixed_max.max(v);
            }
        }
        if fixed_max < fixed_min {
            return Err(invalid("mattes", "no fixed-image samples were observed"));
        }
        if fixed_max - fixed_min <= f64::EPSILON {
            return Err(invalid(
                "mattes",
                "the fixed image is constant over the samples",
            ));
        }
        let mut moving_min = f64::MAX;
        let mut moving_max = f64::MIN;
        for v in moving.data().iter().copied().filter(|v| v.is_finite()) {
            moving_min = moving_min.min(v);
            moving_max = moving_max.max(v);
        }
        if moving_max - moving_min <= f64::EPSILON {
            return Err(invalid("mattes", "the moving image is constant"));
        }
        let span = (bins - 2 * PADDING) as f64;
        let fixed_bin_size = (fixed_max - fixed_min) / span;
        let moving_bin_size = (moving_max - moving_min) / span;
        Ok(Self {
            bins,
            fixed_min,
            fixed_max,
            moving_min,
            moving_max,
            fixed_bin_size,
            moving_bin_size,
            fixed_normalized_min: fixed_min / fixed_bin_size - PADDING as f64,
            moving_normalized_min: moving_min / moving_bin_size - PADDING as f64,
        })
    }

    fn clamp_bin(&self, term: f64) -> usize {
        let idx = term as i64;
        idx.clamp(PADDING as i64, self.bins as i64 - 3) as usize
    }

    /// The fixed-axis (box window) bin of an intensity.
    pub fn fixed_bin(&self, value: f64) -> usize {
        self.clamp_bin(value / self.fixed_bin_size - self.fixed_normalized_min)
    }
}

/// Inputs of one evaluation.
#[derive(Clone, Copy)]
pub struct MiInputs<'a> {
    /// The (smoothed, preprocessed) fixed image.
    pub fixed: &'a Volume<f64>,
    /// The (smoothed, preprocessed) moving image.
    pub moving: &'a Volume<f64>,
    /// Sample points in the virtual domain (physical LPS mm), from
    /// [`crate::sampling::metric_sample_points`].
    pub samples: &'a [Vec3],
    /// Virtual point to fixed image point (identity unless an initial fixed transform exists).
    pub fixed_transform: &'a TransformChain,
    /// Virtual point to moving image point, with its optimised parameters.
    pub moving_transform: &'a LinearMovingTransform,
    /// The fixed-axis bin of each sample, from [`crate::linear::prepare_samples`], valid only
    /// while `fixed_transform` is the identity; `None` interpolates the fixed image per sample.
    pub fixed_bin: Option<&'a [u32]>,
}

/// Result of one evaluation.
#[derive(Clone, Debug)]
pub struct MiEvaluation {
    /// `-MI`.
    pub value: f64,
    /// `+dMI/dtheta` per parameter (empty when not requested).
    pub derivative: Vec<f64>,
    /// Samples that contributed.
    pub num_valid: usize,
    /// Normalised joint PDF, index `fixed_bin * bins + moving_bin`.
    pub joint_pdf: Vec<f64>,
    /// Normalised fixed marginal.
    pub fixed_marginal: Vec<f64>,
    /// Moving marginal (column sums of the joint PDF).
    pub moving_marginal: Vec<f64>,
}

const SAMPLE_CHUNK: usize = 8192;

struct Partial {
    joint: Vec<f64>,
    fixed_marginal: Vec<f64>,
    dpdf: Vec<f64>,
    valid: usize,
}

fn accumulate(
    binning: &MiBinning,
    inp: &MiInputs<'_>,
    samples: &[Vec3],
    fixed_bin: Option<&[u32]>,
    bins: usize,
    params: usize,
    want_derivative: bool,
) -> Partial {
    let mut joint = vec![0.0f64; bins * bins];
    let mut fixed_marginal = vec![0.0f64; bins];
    let mut dpdf = if want_derivative {
        vec![0.0f64; bins * bins * params]
    } else {
        Vec::new()
    };
    let mut valid = 0usize;
    let moving_map = inp.moving_transform.prepared();
    let mut jac = [[0.0f64; 3]; 12];
    for (i, s) in samples.iter().enumerate() {
        let f_index = match fixed_bin {
            Some(bin) => bin[i] as usize,
            None => {
                let Some(fv) = linear_at_point(inp.fixed, inp.fixed_transform.apply(*s)) else {
                    continue;
                };
                if !fv.is_finite() {
                    continue;
                }
                binning.fixed_bin(fv)
            }
        };
        let mp = moving_map.apply(*s);
        let Some(mv) = linear_at_point(inp.moving, mp) else {
            continue;
        };
        if !mv.is_finite() || mv < binning.moving_min || mv > binning.moving_max {
            continue;
        }
        let term = mv / binning.moving_bin_size - binning.moving_normalized_min;
        let m_index = binning.clamp_bin(term);
        fixed_marginal[f_index] += 1.0;
        let (mut grad, n_cols) = if want_derivative {
            (
                central_difference_at_point(inp.moving, mp),
                moving_map.jacobian_columns(*s, &mut jac),
            )
        } else {
            ([0.0; 3], 0)
        };
        if !grad.iter().all(|g| g.is_finite()) {
            grad = [0.0; 3];
        }
        for pdf_moving in (m_index - 1)..=(m_index + 2) {
            let arg = pdf_moving as f64 - term;
            let cell = f_index * bins + pdf_moving;
            joint[cell] += cubic_bspline(arg);
            if want_derivative {
                let db = cubic_bspline_derivative(arg);
                for (mu, col) in jac[..n_cols].iter().enumerate() {
                    let inner = col[0] * grad[0] + col[1] * grad[1] + col[2] * grad[2];
                    dpdf[cell * params + mu] += inner * db;
                }
            }
        }
        valid += 1;
    }
    Partial {
        joint,
        fixed_marginal,
        dpdf,
        valid,
    }
}

impl MattesMi {
    /// Evaluates the metric. A sample contributes when its mapped fixed and moving points lie
    /// inside the image buffers and the interpolated moving value is within the moving image's
    /// intensity range. Fails when no sample contributes or the joint PDF sums to zero.
    pub fn evaluate(
        &self,
        binning: &MiBinning,
        inp: &MiInputs<'_>,
        want_derivative: bool,
    ) -> Result<MiEvaluation> {
        let bins = self.bins;
        let params = inp.moving_transform.optimised.number_of_parameters();
        let partials: Vec<Partial> = inp
            .samples
            .par_chunks(SAMPLE_CHUNK)
            .enumerate()
            .map(|(c, chunk)| {
                let start = c * SAMPLE_CHUNK;
                let bin = inp.fixed_bin.map(|b| &b[start..start + chunk.len()]);
                accumulate(binning, inp, chunk, bin, bins, params, want_derivative)
            })
            .collect();
        let mut joint = vec![0.0f64; bins * bins];
        let mut fixed_marginal = vec![0.0f64; bins];
        let mut dpdf = if want_derivative {
            vec![0.0f64; bins * bins * params]
        } else {
            Vec::new()
        };
        let mut num_valid = 0usize;
        for p in &partials {
            for (a, b) in joint.iter_mut().zip(&p.joint) {
                *a += b;
            }
            for (a, b) in fixed_marginal.iter_mut().zip(&p.fixed_marginal) {
                *a += b;
            }
            for (a, b) in dpdf.iter_mut().zip(&p.dpdf) {
                *a += b;
            }
            num_valid += p.valid;
        }
        let fin = finish_histogram(
            bins,
            binning.moving_bin_size,
            num_valid,
            joint,
            fixed_marginal,
            want_derivative,
        )?;
        let mut derivative = if want_derivative {
            vec![0.0f64; params]
        } else {
            Vec::new()
        };
        if want_derivative {
            for (cell, r) in fin.ratio.iter().enumerate() {
                if *r != 0.0 {
                    for mu in 0..params {
                        derivative[mu] += dpdf[cell * params + mu] * r;
                    }
                }
            }
        }
        Ok(MiEvaluation {
            value: fin.value,
            derivative,
            num_valid,
            joint_pdf: fin.joint_pdf,
            fixed_marginal: fin.fixed_marginal,
            moving_marginal: fin.moving_marginal,
        })
    }
}

/// Result of [`finish_histogram`].
#[derive(Clone, Debug)]
pub struct HistogramResult {
    /// `-MI`.
    pub value: f64,
    /// Per joint cell (`fixed_bin * bins + moving_bin`): `nFactor * log(p(f,m) / p_M(m))` for
    /// cells that ITK includes in the sum, zero for the others. The derivative of the metric is
    /// `sum_cell dpdf[cell] * ratio[cell]`. Empty when not requested.
    pub ratio: Vec<f64>,
    /// Normalised joint PDF.
    pub joint_pdf: Vec<f64>,
    /// Normalised fixed marginal.
    pub fixed_marginal: Vec<f64>,
    /// Moving marginal (column sums of the joint PDF).
    pub moving_marginal: Vec<f64>,
}

/// ITK's `ComputeResults` for a global transform from the accumulated, un-normalised joint
/// histogram and fixed marginal of `num_valid` samples: Kahan-summed normalisation, marginals,
/// the double sum over the histogram and the per-cell derivative weights.
pub fn finish_histogram(
    bins: usize,
    moving_bin_size: f64,
    num_valid: usize,
    mut joint: Vec<f64>,
    mut fixed_marginal: Vec<f64>,
    want_ratio: bool,
) -> Result<HistogramResult> {
    if num_valid == 0 {
        return Err(invalid(
            "mattes",
            "all samples map outside the moving image buffer",
        ));
    }
    let mut pdf_sum = Kahan::new();
    for v in &joint {
        pdf_sum.add(*v);
    }
    if pdf_sum.sum < f64::EPSILON {
        return Err(invalid("mattes", "joint PDF summed to zero"));
    }
    let norm = 1.0 / pdf_sum.sum;
    for v in &mut joint {
        *v *= norm;
    }
    for v in &mut fixed_marginal {
        *v *= norm;
    }
    let mut moving_marginal = vec![0.0f64; bins];
    for (col, mm) in moving_marginal.iter_mut().enumerate() {
        for row in 0..bins {
            *mm += joint[row * bins + col];
        }
    }
    let close_to_zero = f64::EPSILON;
    let n_factor = -1.0 / (moving_bin_size * num_valid as f64);
    let mut ratio = if want_ratio {
        vec![0.0f64; bins * bins]
    } else {
        Vec::new()
    };
    let mut sum = 0.0;
    for f in 0..bins {
        let fm = fixed_marginal[f];
        if fm <= close_to_zero {
            continue;
        }
        let log_fm = fm.ln();
        for m in 0..bins {
            let mm = moving_marginal[m];
            let jv = joint[f * bins + m];
            if mm > close_to_zero && jv > close_to_zero {
                let p_ratio = (jv / mm).ln();
                sum += jv * (p_ratio - log_fm);
                if want_ratio {
                    ratio[f * bins + m] = n_factor * p_ratio;
                }
            }
        }
    }
    Ok(HistogramResult {
        value: -sum,
        ratio,
        joint_pdf: joint,
        fixed_marginal,
        moving_marginal,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_range_loop)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::linalg::IDENTITY3;
    use crate::sampling::{metric_sample_points, SamplingSpec, SamplingStrategy, SeedSequence};
    use crate::transform::{AffineTransform, ParametricTransform};

    fn grid(n: usize) -> Grid {
        Grid::new([n, n, n], [1.0; 3], [0.0; 3], IDENTITY3).unwrap()
    }

    fn smooth(g: &Grid, shift: Vec3, phase: f64) -> Volume<f64> {
        let mut data = vec![0.0; g.num_voxels()];
        for idx in 0..g.num_voxels() {
            let [i, j, k] = g.ijk(idx);
            let (x, y, z) = (
                i as f64 - shift[0],
                j as f64 - shift[1],
                k as f64 - shift[2],
            );
            data[idx] = 1.0
                + (0.35 * x + phase).sin() * (0.3 * y).cos()
                + 0.6 * (0.25 * z + 0.15 * x).sin();
        }
        Volume::from_vec(*g, data).unwrap()
    }

    fn identity_moving() -> LinearMovingTransform {
        LinearMovingTransform {
            outer: AffineTransform::identity(),
            optimised: ParametricTransform::euler3d([0.0; 6], [0.0; 3]),
        }
    }

    fn samples(g: &Grid) -> Vec<Vec3> {
        metric_sample_points(
            g,
            SamplingSpec {
                strategy: SamplingStrategy::Regular,
                percentage: 0.25,
            },
            &mut SeedSequence::new(20_260_910),
            None,
        )
    }

    #[test]
    fn cubic_bspline_kernels_partition_unity_and_derivative_matches_finite_differences() {
        for t in [0.0, 0.1, 0.37, 0.5, 0.99] {
            let w: f64 = (-1..=2).map(|k| cubic_bspline(f64::from(k) - t)).sum();
            assert!((w - 1.0).abs() < 1e-12);
            let dw: f64 = (-1..=2)
                .map(|k| cubic_bspline_derivative(f64::from(k) - t))
                .sum();
            assert!(dw.abs() < 1e-12);
        }
        for u in [-1.7, -1.2, -0.6, -0.1, 0.2, 0.9, 1.4, 1.9] {
            let h = 1e-6;
            let fd = (cubic_bspline(u + h) - cubic_bspline(u - h)) / (2.0 * h);
            assert!((fd - cubic_bspline_derivative(u)).abs() < 1e-6, "u {u}");
        }
        assert_eq!(cubic_bspline(2.0), 0.0);
        assert!((cubic_bspline(0.0) - 4.0 / 6.0).abs() < 1e-15);
    }

    #[test]
    fn binning_follows_the_itk_padding_rule() {
        let g = grid(12);
        let f = smooth(&g, [0.0; 3], 0.0);
        let m = smooth(&g, [0.5, 0.0, 0.0], 0.3);
        let s = samples(&g);
        let b = MiBinning::new(&f, &m, &s, &TransformChain::new(), 32).unwrap();
        assert!((b.fixed_bin_size - (b.fixed_max - b.fixed_min) / 28.0).abs() < 1e-15);
        assert!((b.moving_normalized_min - (b.moving_min / b.moving_bin_size - 2.0)).abs() < 1e-12);
        assert_eq!(b.fixed_bin(b.fixed_min), 2);
        assert_eq!(b.fixed_bin(b.fixed_max), 29);
        assert_eq!(b.fixed_bin(b.fixed_min - 100.0), 2);
        assert_eq!(b.fixed_bin(b.fixed_max + 100.0), 29);
        assert!(b.moving_min <= m.data().iter().copied().fold(f64::MAX, f64::min));
        assert!(MiBinning::new(&f, &m, &s, &TransformChain::new(), 4).is_err());
    }

    #[test]
    fn pdf_is_normalised_and_the_value_is_minus_the_mutual_information() {
        let g = grid(14);
        let f = smooth(&g, [0.0; 3], 0.0);
        let m = smooth(&g, [0.7, -0.4, 0.2], 0.2);
        let s = samples(&g);
        let id = TransformChain::new();
        let mv = identity_moving();
        let b = MiBinning::new(&f, &m, &s, &id, 32).unwrap();
        let e = MattesMi { bins: 32 }
            .evaluate(
                &b,
                &MiInputs {
                    fixed: &f,
                    moving: &m,
                    samples: &s,
                    fixed_transform: &id,
                    moving_transform: &mv,
                    fixed_bin: None,
                },
                false,
            )
            .unwrap();
        let total: f64 = e.joint_pdf.iter().sum();
        assert!((total - 1.0).abs() < 1e-12);
        assert!((e.fixed_marginal.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((e.moving_marginal.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        let mut mi = 0.0;
        for fi in 0..32 {
            for mi_bin in 0..32 {
                let p = e.joint_pdf[fi * 32 + mi_bin];
                if p > 1e-12 {
                    mi += p * (p / (e.fixed_marginal[fi] * e.moving_marginal[mi_bin])).ln();
                }
            }
        }
        assert!((e.value + mi).abs() < 1e-9);
        assert!(mi > 0.0);
        assert!(e.num_valid > 0 && e.num_valid <= s.len());
        for fi in 0..32 {
            let row: f64 = (0..32).map(|c| e.joint_pdf[fi * 32 + c]).sum();
            assert!((row - e.fixed_marginal[fi]).abs() < 1e-12);
        }
    }

    #[test]
    fn registered_images_have_more_mutual_information_than_a_shifted_pair() {
        let g = grid(16);
        let f = smooth(&g, [0.0; 3], 0.0);
        let s = samples(&g);
        let id = TransformChain::new();
        let b = MiBinning::new(&f, &f, &s, &id, 32).unwrap();
        let eval = |t: Vec3| {
            let mv = LinearMovingTransform {
                outer: AffineTransform::identity(),
                optimised: ParametricTransform::euler3d(
                    [0.0, 0.0, 0.0, t[0], t[1], t[2]],
                    [0.0; 3],
                ),
            };
            MattesMi { bins: 32 }
                .evaluate(
                    &b,
                    &MiInputs {
                        fixed: &f,
                        moving: &f,
                        samples: &s,
                        fixed_transform: &id,
                        moving_transform: &mv,
                        fixed_bin: None,
                    },
                    false,
                )
                .unwrap()
                .value
        };
        let aligned = eval([0.0; 3]);
        assert!(aligned < eval([1.5, 0.0, 0.0]));
        assert!(aligned < eval([0.0, -2.0, 1.0]));
    }

    #[test]
    fn invariant_to_affine_intensity_remaps_and_close_for_a_monotone_nonlinear_one() {
        let g = grid(14);
        let f = smooth(&g, [0.0; 3], 0.0);
        let m = smooth(&g, [0.6, 0.3, -0.2], 0.1);
        let s = samples(&g);
        let id = TransformChain::new();
        let mv = identity_moving();
        let run = |a: &Volume<f64>, b: &Volume<f64>| {
            let bin = MiBinning::new(a, b, &s, &id, 32).unwrap();
            MattesMi { bins: 32 }
                .evaluate(
                    &bin,
                    &MiInputs {
                        fixed: a,
                        moving: b,
                        samples: &s,
                        fixed_transform: &id,
                        moving_transform: &mv,
                        fixed_bin: None,
                    },
                    false,
                )
                .unwrap()
                .value
        };
        let base = run(&f, &m);
        let m2 = Volume::from_vec(g, m.data().iter().map(|v| 3.0 * v + 11.0).collect()).unwrap();
        let f2 = Volume::from_vec(g, f.data().iter().map(|v| 0.5 * v - 4.0).collect()).unwrap();
        assert!((run(&f, &m2) - base).abs() < 1e-9);
        assert!((run(&f2, &m) - base).abs() < 1e-9);
        let m3 =
            Volume::from_vec(g, m.data().iter().map(|v| (v + 1.0).powf(1.5)).collect()).unwrap();
        let rel = (run(&f, &m3) - base).abs() / base.abs();
        assert!(rel < 0.15, "{rel}");
    }

    fn fd_check(moving: LinearMovingTransform, tolerance: f64) {
        let g = grid(26);
        let f = smooth(&g, [0.0; 3], 0.0);
        let m = smooth(&g, [1.1, -0.8, 0.6], 0.0);
        let interior = |p: Vec3| p.iter().all(|c| (6.0..=19.0).contains(c));
        let s = metric_sample_points(
            &g,
            SamplingSpec {
                strategy: SamplingStrategy::Regular,
                percentage: 0.25,
            },
            &mut SeedSequence::new(20_260_910),
            Some(&interior),
        );
        let id = TransformChain::new();
        let b = MiBinning::new(&f, &m, &s, &id, 32).unwrap();
        let metric = MattesMi { bins: 32 };
        let run = |t: &LinearMovingTransform, d: bool| {
            metric
                .evaluate(
                    &b,
                    &MiInputs {
                        fixed: &f,
                        moving: &m,
                        samples: &s,
                        fixed_transform: &id,
                        moving_transform: t,
                        fixed_bin: None,
                    },
                    d,
                )
                .unwrap()
        };
        let base = run(&moving, true);
        let params = moving.optimised.parameters();
        let mut max_fd = 0.0f64;
        let mut fd = vec![0.0; params.len()];
        for k in 0..params.len() {
            let h = if k < 3 && params.len() == 6 {
                1e-4
            } else {
                1e-3
            };
            let mut up = params.clone();
            up[k] += h;
            let mut dn = params.clone();
            dn[k] -= h;
            let vu = run(&moving.with_parameters(&up).unwrap(), false);
            let vd = run(&moving.with_parameters(&dn).unwrap(), false);
            assert_eq!(vu.num_valid, base.num_valid);
            assert_eq!(vd.num_valid, base.num_valid);
            fd[k] = -(vu.value - vd.value) / (2.0 * h);
            max_fd = max_fd.max(fd[k].abs());
        }
        assert!(max_fd > 1e-4);
        for k in 0..params.len() {
            assert!(
                (base.derivative[k] - fd[k]).abs() <= tolerance * max_fd,
                "param {k}: analytic {} vs finite difference {} (max {max_fd})",
                base.derivative[k],
                fd[k]
            );
        }
    }

    #[test]
    fn rigid_derivative_matches_central_differences_of_the_value() {
        fd_check(
            LinearMovingTransform {
                outer: AffineTransform::identity(),
                optimised: ParametricTransform::euler3d(
                    [0.01, -0.02, 0.015, 0.4, -0.3, 0.2],
                    [13.0, 13.0, 13.0],
                ),
            },
            0.08,
        );
    }

    #[test]
    fn affine_derivative_matches_central_differences_with_an_outer_transform() {
        let base = AffineTransform::new(
            [[1.0, 0.01, 0.0], [-0.01, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [0.3, -0.2, 0.1],
            [13.0, 13.0, 13.0],
        );
        fd_check(
            LinearMovingTransform {
                outer: AffineTransform::from_translation([0.1, 0.05, -0.1]),
                optimised: ParametricTransform::affine(&base),
            },
            0.08,
        );
    }

    #[test]
    fn errors_for_no_overlap_and_constant_images() {
        let g = grid(10);
        let f = smooth(&g, [0.0; 3], 0.0);
        let s = samples(&g);
        let id = TransformChain::new();
        let b = MiBinning::new(&f, &f, &s, &id, 32).unwrap();
        let far = LinearMovingTransform {
            outer: AffineTransform::from_translation([500.0, 0.0, 0.0]),
            optimised: ParametricTransform::euler3d([0.0; 6], [0.0; 3]),
        };
        let r = MattesMi { bins: 32 }.evaluate(
            &b,
            &MiInputs {
                fixed: &f,
                moving: &f,
                samples: &s,
                fixed_transform: &id,
                moving_transform: &far,
                fixed_bin: None,
            },
            true,
        );
        assert!(r.is_err());
        let flat = Volume::from_vec(g, vec![1.0; g.num_voxels()]).unwrap();
        assert!(MiBinning::new(&flat, &f, &s, &id, 32).is_err());
        assert!(MiBinning::new(&f, &flat, &s, &id, 32).is_err());
    }

    #[test]
    fn non_finite_voxels_are_skipped_and_the_fixed_marginal_still_sums_to_one() {
        let g = grid(14);
        let f = smooth(&g, [0.0; 3], 0.0);
        let m = smooth(&g, [0.7, -0.4, 0.2], 0.2);
        let s = samples(&g);
        let id = TransformChain::new();
        let mv = identity_moving();
        let clean = MiBinning::new(&f, &m, &s, &id, 32).unwrap();
        let run = |f: &Volume<f64>, m: &Volume<f64>, bin: &MiBinning| {
            MattesMi { bins: 32 }
                .evaluate(
                    bin,
                    &MiInputs {
                        fixed: f,
                        moving: m,
                        samples: &s,
                        fixed_transform: &id,
                        moving_transform: &mv,
                        fixed_bin: None,
                    },
                    true,
                )
                .unwrap()
        };
        let base = run(&f, &m, &clean);

        let c = g.point_to_index(s[s.len() / 2]);
        let at = g.linear_index(
            c[0].round() as usize,
            c[1].round() as usize,
            c[2].round() as usize,
        );
        let mut fd = f.data().to_vec();
        let mut md = m.data().to_vec();
        fd[at] = f64::NAN;
        let mi = (at + 3) % md.len();
        md[mi] = f64::NAN;
        let (fnan, mnan) = (
            Volume::from_vec(g, fd).unwrap(),
            Volume::from_vec(g, md).unwrap(),
        );
        let b = MiBinning::new(&fnan, &mnan, &s, &id, 32).unwrap();
        assert!(b.fixed_min.is_finite() && b.fixed_max.is_finite());
        assert!(b.moving_min.is_finite() && b.moving_max.is_finite());
        let e = run(&fnan, &mnan, &b);
        assert!(
            e.num_valid < base.num_valid,
            "{} {}",
            e.num_valid,
            base.num_valid
        );
        assert!(e.value.is_finite() && e.derivative.iter().all(|d| d.is_finite()));
        assert!((e.fixed_marginal.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((e.joint_pdf.iter().sum::<f64>() - 1.0).abs() < 1e-12);

        let prepared = crate::linear::prepare_samples(&fnan, &mnan, &s, 32).unwrap();
        assert!(prepared.points.len() < s.len());
        assert_eq!(prepared.points.len(), prepared.fixed_bin.len());
    }
}
