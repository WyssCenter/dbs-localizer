// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Linear interpolation percentile (NumPy `percentile` default ``linear``), NaNs ignored.

/// `q` in `[0, 100]`. Returns `NaN` if no finite samples.
pub fn percentile_linear(samples: &[f64], q: f64) -> f64 {
    let mut v: Vec<f64> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 1 {
        return v[0];
    }
    let rank = (q / 100.0) * (n - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return v[lo];
    }
    let w = rank - lo as f64;
    v[lo] * (1.0 - w) + v[hi] * w
}
