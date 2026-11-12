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
// Portions derived from ANTs `itkantsRegistrationHelper.hxx` (`PreprocessImage`: 256-bin
// histogram, marginal scale 10, quantile window, windowing to [0, 1]; the call site in
// `RegistrationHelper::SetUpRegistration`) and from ITK `itkImageToHistogramFilter.hxx`
// (`InitializeOutputHistogram`, `ApplyMarginalScale`, `ThreadedStreamedGenerateData`),
// `itkHistogram.hxx` (`Initialize` with `float` bin intervals, `GetIndex`, `Quantile`) and
// `itkIntensityWindowingImageFilter.h` (`IntensityWindowingTransform`). See
// `docs/THIRD_PARTY_PORTS.md`.

//! `antsRegistration` intensity preprocessing: winsorise by histogram quantiles and rescale.

use rayon::prelude::*;

use crate::error::{invalid, Result};
use crate::volume::Volume;

const HISTOGRAM_BINS: usize = 256;
const MARGINAL_SCALE: f64 = 10.0;

struct Histogram {
    min: Vec<f64>,
    max: Vec<f64>,
    freq: Vec<f64>,
}

impl Histogram {
    fn build(values: &[f64]) -> Result<Self> {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        let mut finite = 0usize;
        for v in values {
            if !v.is_finite() {
                continue;
            }
            finite += 1;
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        if finite == 0 {
            return Err(invalid("preprocess", "image has no finite values"));
        }
        let margin = ((hi - lo) / HISTOGRAM_BINS as f64) / MARGINAL_SCALE;
        let upper = hi + margin;
        let interval = (upper as f32 - lo as f32) / HISTOGRAM_BINS as f32;
        let mut min = vec![0.0; HISTOGRAM_BINS];
        let mut max = vec![0.0; HISTOGRAM_BINS];
        for j in 0..HISTOGRAM_BINS - 1 {
            min[j] = lo + f64::from(j as f32 * interval);
            max[j] = lo + f64::from((j as f32 + 1.0) * interval);
        }
        min[HISTOGRAM_BINS - 1] = lo + f64::from((HISTOGRAM_BINS as f32 - 1.0) * interval);
        max[HISTOGRAM_BINS - 1] = upper;
        let mut h = Self {
            min,
            max,
            freq: vec![0.0; HISTOGRAM_BINS],
        };
        let counts = values
            .par_iter()
            .fold(
                || vec![0u64; HISTOGRAM_BINS],
                |mut acc, v| {
                    if v.is_finite() {
                        acc[h.index_of(*v)] += 1;
                    }
                    acc
                },
            )
            .reduce(
                || vec![0u64; HISTOGRAM_BINS],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b) {
                        *x += y;
                    }
                    a
                },
            );
        for (f, c) in h.freq.iter_mut().zip(counts) {
            *f = c as f64;
        }
        Ok(h)
    }

    fn index_of(&self, v: f64) -> usize {
        if v < self.min[0] {
            return 0;
        }
        let n = self.min.len();
        if v >= self.max[n - 1] {
            return n - 1;
        }
        self.min.partition_point(|m| *m <= v).saturating_sub(1)
    }

    fn quantile(&self, p: f64) -> f64 {
        let size = self.freq.len();
        let total: f64 = self.freq.iter().sum();
        let mut cumulated = 0.0;
        if p < 0.5 {
            let mut n = 0usize;
            let mut p_n = 0.0;
            let mut p_prev;
            let mut f_n;
            loop {
                f_n = self.freq[n];
                cumulated += f_n;
                p_prev = p_n;
                p_n = cumulated / total;
                n += 1;
                if !(n < size && p_n < p) {
                    break;
                }
            }
            let proportion = f_n / total;
            let min = self.min[n - 1];
            let interval = self.max[n - 1] - min;
            min + ((p - p_prev) / proportion) * interval
        } else {
            let mut n = size as i64 - 1;
            let mut m = 0usize;
            let mut p_n = 1.0;
            let mut p_prev;
            let mut f_n;
            loop {
                f_n = self.freq[n as usize];
                cumulated += f_n;
                p_prev = p_n;
                p_n = 1.0 - cumulated / total;
                n -= 1;
                m += 1;
                if !(m < size && p_n > p) {
                    break;
                }
            }
            let proportion = f_n / total;
            let bin = (n + 1) as usize;
            let max = self.max[bin];
            let interval = max - self.min[bin];
            max - ((p_prev - p) / proportion) * interval
        }
    }
}

/// The two window bounds `PreprocessImage` derives from the histogram: the lower and upper
/// quantile values, each rounded to `f32` (ITK stores them in `float` variables).
///
/// With quantiles `(0, 1)` the upper bound is not the image maximum but
/// `max + (max - min) / 2560` (the histogram's marginal-scale margin).
pub fn quantile_window(values: &[f64], lower: f32, upper: f32) -> Result<(f64, f64)> {
    let h = Histogram::build(values)?;
    let lo = h.quantile(f64::from(lower)) as f32;
    let hi = h.quantile(f64::from(upper)) as f32;
    Ok((f64::from(lo), f64::from(hi)))
}

/// `PreprocessImage` without histogram matching (every `antsRegistration` recipe of the cohort pipeline passes
/// `--use-histogram-matching 0`): linear map of `[lo, hi]` (the quantile window) onto
/// `[out_min, out_max]`, values outside the window clamped to the output bounds.
///
/// `antsRegistration` always runs this on every metric image, with quantiles `(0, 1)` unless
/// `--winsorize-image-intensities [lower,upper]` is given, and the output range `[0, 1]`.
/// Non-finite voxels are ignored when the window is computed (`std::min`/`std::max` semantics of
/// ITK `ImageToHistogramFilter`) and pass through the map unchanged, like
/// `IntensityWindowingTransform`; fails for an all-non-finite image and for a degenerate window
/// (`hi <= lo`, e.g. a constant image).
pub fn preprocess_values(
    values: &[f64],
    lower_quantile: f32,
    upper_quantile: f32,
    out_min: f64,
    out_max: f64,
) -> Result<Vec<f64>> {
    let (lo, hi) = quantile_window(values, lower_quantile, upper_quantile)?;
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        return Err(invalid(
            "preprocess",
            format!("degenerate intensity window [{lo}, {hi}]"),
        ));
    }
    let scale = (out_max - out_min) / (hi - lo);
    let shift = out_min - lo * scale;
    Ok(values
        .iter()
        .map(|x| {
            if *x < lo {
                out_min
            } else if *x > hi {
                out_max
            } else {
                x * scale + shift
            }
        })
        .collect())
}

/// [`preprocess_values`] on a [`Volume`], keeping its grid.
pub fn preprocess_volume(
    vol: &Volume<f64>,
    lower_quantile: f32,
    upper_quantile: f32,
    out_min: f64,
    out_max: f64,
) -> Result<Volume<f64>> {
    let data = preprocess_values(vol.data(), lower_quantile, upper_quantile, out_min, out_max)?;
    Volume::from_vec(*vol.grid(), data)
}

/// The `PreprocessImage` of a stage whose argv omits `--winsorize-image-intensities`: the helper
/// holds `m_WinsorizeImageIntensities = false`, `m_LowerQuantile = 0.0`, `m_UpperQuantile = 1.0`,
/// so the quantile window is `(0, 1)` and the output range `[0, 1]`. The CT → anchor coreg argv
/// omits the flag, so this is the preprocessing that stage runs. Histogram matching is off: the
/// `antsRegistration` CLI initialises `UseHistogramMatching` to false
/// (`antsRegistrationTemplateHeader.h`) and no cohort recipe passes it.
pub fn preprocess_volume_no_winsorize(vol: &Volume<f64>) -> Result<Volume<f64>> {
    preprocess_volume(vol, 0.0, 1.0, 0.0, 1.0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64).collect()
    }

    #[test]
    fn no_winsorizing_maps_min_to_zero_and_max_just_below_one() {
        let v = ramp(1000);
        let out = preprocess_values(&v, 0.0, 1.0, 0.0, 1.0).unwrap();
        assert_eq!(out[0], 0.0);
        let want_max = 999.0 / (999.0 + 999.0 / 2560.0);
        assert!((out[999] - want_max).abs() < 1e-6, "{}", out[999]);
        assert!(out.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn winsorizing_clamps_the_tails_and_is_affine_inside() {
        let mut v = ramp(1000);
        v[3] = -5000.0;
        v[997] = 9000.0;
        let out = preprocess_values(&v, 0.005, 0.995, 0.0, 1.0).unwrap();
        assert_eq!(out[3], 0.0);
        assert_eq!(out[997], 1.0);
        let (lo, hi) = quantile_window(&v, 0.005, 0.995).unwrap();
        assert!(lo > -5000.0 && lo < 100.0 && hi > 500.0 && hi < 9000.0);
        let mid = (500.0 - lo) / (hi - lo);
        assert!((out[500] - mid).abs() < 1e-12);
    }

    #[test]
    fn quantile_interpolates_inside_the_bin_like_itk() {
        let v = ramp(2560);
        let (lo, hi) = quantile_window(&v, 0.25, 0.75).unwrap();
        assert!((lo - 640.0).abs() < 5.0, "{lo}");
        assert!((hi - 1920.0).abs() < 5.0, "{hi}");
    }

    #[test]
    fn output_range_and_degenerate_inputs() {
        let v = ramp(100);
        let out = preprocess_values(&v, 0.0, 1.0, -1.0, 3.0).unwrap();
        assert_eq!(out[0], -1.0);
        assert!(out[99] < 3.0 && out[99] > 2.99);
        assert!(preprocess_values(&[2.0; 10], 0.0, 1.0, 0.0, 1.0).is_err());
        assert!(preprocess_values(&[], 0.0, 1.0, 0.0, 1.0).is_err());
        let mixed = preprocess_values(&[1.0, 3.0, f64::NAN], 0.0, 1.0, 0.0, 1.0).unwrap();
        assert_eq!(mixed[0], 0.0);
        assert!(mixed[1] > 0.99 && mixed[1] < 1.0);
        assert!(mixed[2].is_nan());
        assert!(preprocess_values(&[f64::NAN, f64::INFINITY], 0.0, 1.0, 0.0, 1.0).is_err());
    }

    #[test]
    fn invariant_to_affine_intensity_changes() {
        let v: Vec<f64> = (0..500).map(|i| ((i * 37) % 101) as f64).collect();
        let w: Vec<f64> = v.iter().map(|x| 3.0 * x + 40.0).collect();
        let a = preprocess_values(&v, 0.005, 0.995, 0.0, 1.0).unwrap();
        let b = preprocess_values(&w, 0.005, 0.995, 0.0, 1.0).unwrap();
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-5);
        }
    }

    #[test]
    fn the_coreg_path_uses_the_unwinsorized_quantile_window() {
        use crate::grid::Grid;
        use crate::linalg::IDENTITY3;
        use crate::volume::Volume;
        let g = Grid::new([102, 1, 1], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let mut values: Vec<f64> = (0..=100).map(f64::from).collect();
        values.push(9000.0);
        let v = Volume::from_vec(g, values).unwrap();
        let out = preprocess_volume_no_winsorize(&v).unwrap();
        let direct = preprocess_values(v.data(), 0.0, 1.0, 0.0, 1.0).unwrap();
        assert_eq!(out.data(), direct.as_slice());
        let (lo, hi) = quantile_window(v.data(), 0.0, 1.0).unwrap();
        assert_eq!(lo, 0.0);
        assert!(hi > 9000.0);
        let wins = preprocess_volume(&v, 0.005, 0.995, 0.0, 1.0).unwrap();
        assert_eq!(wins.data()[101], 1.0);
        assert!(out.data()[101] < 1.0);
    }
}
