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
// Portions derived from ITK `itkWindowConvergenceMonitoringFunction.hxx` (`AddEnergyValue`,
// `GetConvergenceValue`) together with the least-squares fit and gradient evaluation it
// delegates to: `itkBSplineScatteredDataPointSetToImageFilter.hxx`
// (`ThreadedGenerateDataForFitting`, `AfterThreadedGenerateData`: per-point weights, delta and
// omega lattices, `phi = delta / omega`) and `itkBSplineControlPointImageFunction.hxx`
// (`EvaluateGradient`). The order-1, two-control-point fit is written in closed form. See
// `docs/THIRD_PARTY_PORTS.md`.

//! ITK's window convergence monitor (`WindowConvergenceMonitoringFunction`).

use std::collections::VecDeque;

const SPLINE_EPSILON: f64 = 1e-3;
const DOMAIN_SPACING: f64 = 0.1;
const DOMAIN_SIZE: f64 = 11.0;

/// ITK's `WindowConvergenceMonitoringFunction<double>`.
///
/// Energy values are pushed one per iteration; only the last `window` values are kept, but the
/// normalising total is the running sum of the absolute value of **every** value added since
/// construction or [`WindowConvergenceMonitor::clear`]. [`WindowConvergenceMonitor::convergence_value`]
/// is `f64::MAX` until `window` values are present; then it is `phi_0 - phi_1` for the order-1
/// B-spline with two control points fitted by ITK's scattered-data least squares to the points
/// `(n / (window - 1), e_n / total)`. The optimizer stops when it is `<=` the threshold
/// (`antsRegistration` convergence value `1e-6`); `SyNImageRegistrationMethod` when it is `<`.
#[derive(Clone, Debug)]
pub struct WindowConvergenceMonitor {
    window: usize,
    values: VecDeque<f64>,
    total_energy: f64,
}

impl WindowConvergenceMonitor {
    /// A monitor with the given window size (`antsRegistration`'s third convergence parameter).
    pub fn new(window: usize) -> Self {
        Self {
            window,
            values: VecDeque::with_capacity(window + 1),
            total_energy: 0.0,
        }
    }

    /// Adds one energy (metric) value.
    pub fn add_energy_value(&mut self, value: f64) {
        self.values.push_back(value);
        if self.values.len() > self.window {
            self.values.pop_front();
        }
        self.total_energy += value.abs();
    }

    /// Forgets all values and the running total.
    pub fn clear(&mut self) {
        self.values.clear();
        self.total_energy = 0.0;
    }

    /// Number of values currently in the window.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no value has been added.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The convergence value; see the type documentation.
    pub fn convergence_value(&self) -> f64 {
        if self.values.len() < self.window {
            return f64::MAX;
        }
        if self.window < 2 {
            return 0.0;
        }
        let r = 1.0 / ((DOMAIN_SIZE - 1.0) * DOMAIN_SPACING);
        let epsilon = r * DOMAIN_SPACING * SPLINE_EPSILON;
        let mut delta = [0.0f64; 2];
        let mut omega = [0.0f64; 2];
        for (n, e) in self.values.iter().enumerate() {
            let mut p = (n as f64 / (self.window - 1) as f64) * r;
            if (p - 1.0).abs() <= epsilon {
                p = 1.0 - epsilon;
            }
            if p < 0.0 && p.abs() <= epsilon {
                p = 0.0;
            }
            let t = p - p.floor();
            let w = [1.0 - t, t];
            let w2_sum = w[0] * w[0] + w[1] * w[1];
            let v = e / self.total_energy;
            for k in 0..2 {
                omega[k] += w[k] * w[k];
                delta[k] += v * (w[k] * w[k] * w[k] / w2_sum);
            }
        }
        let mut phi = [0.0f64; 2];
        for k in 0..2 {
            if omega[k].abs() > f64::EPSILON {
                let q = delta[k] / omega[k];
                phi[k] = if q.is_finite() { q } else { 0.0 };
            }
        }
        phi[0] - phi[1]
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn not_converged_until_the_window_is_full() {
        let mut m = WindowConvergenceMonitor::new(10);
        for i in 0..9 {
            m.add_energy_value(-1.0 - f64::from(i));
            assert_eq!(m.convergence_value(), f64::MAX);
        }
        m.add_energy_value(-10.0);
        assert!(m.convergence_value() < f64::MAX);
    }

    #[test]
    fn a_flat_profile_is_essentially_zero() {
        let mut m = WindowConvergenceMonitor::new(10);
        for _ in 0..10 {
            m.add_energy_value(-0.5);
        }
        assert!(m.convergence_value().abs() < 1e-4);
    }

    #[test]
    fn a_falling_profile_is_positive_grows_with_the_slope_and_ignores_the_energy_scale() {
        let profile = |slope: f64, scale: f64| {
            let mut m = WindowConvergenceMonitor::new(10);
            for n in 0..10 {
                m.add_energy_value(scale * (100.0 - slope * f64::from(n)));
            }
            m.convergence_value()
        };
        let gentle = profile(0.5, 1.0);
        let steep = profile(2.0, 1.0);
        assert!(gentle > 0.0 && steep > gentle);
        assert!((profile(2.0, 37.0) - steep).abs() < 1e-14);
        assert!(profile(-1.0, 1.0) < 0.0);
    }

    #[test]
    fn total_energy_counts_values_that_left_the_window() {
        let mut a = WindowConvergenceMonitor::new(10);
        let mut b = WindowConvergenceMonitor::new(10);
        for _ in 0..50 {
            a.add_energy_value(-1000.0);
        }
        for n in 0..10 {
            a.add_energy_value(-5.0 + f64::from(n) * 0.01);
            b.add_energy_value(-5.0 + f64::from(n) * 0.01);
        }
        assert_eq!(a.len(), 10);
        assert!(a.convergence_value().abs() < b.convergence_value().abs() * 0.05);
        a.clear();
        assert!(a.is_empty());
        assert_eq!(a.convergence_value(), f64::MAX);
    }

    #[test]
    fn matches_the_closed_form_of_the_two_control_point_fit() {
        let mut m = WindowConvergenceMonitor::new(10);
        let vals = [9.0, 8.5, 8.2, 8.0, 7.9, 7.85, 7.82, 7.80, 7.79, 7.785];
        for v in vals {
            m.add_energy_value(v);
        }
        let total: f64 = vals.iter().sum();
        let eps = 1e-4;
        let (mut d0, mut d1, mut o0, mut o1) = (0.0, 0.0, 0.0, 0.0);
        for (n, v) in vals.iter().enumerate() {
            let mut p = n as f64 / 9.0;
            if (p - 1.0).abs() <= eps {
                p = 1.0 - eps;
            }
            let (t0, t1) = (1.0 - p, p);
            let s = t0 * t0 + t1 * t1;
            let y = v / total;
            o0 += t0 * t0;
            o1 += t1 * t1;
            d0 += y * t0.powi(3) / s;
            d1 += y * t1.powi(3) / s;
        }
        let want = d0 / o0 - d1 / o1;
        assert!((m.convergence_value() - want).abs() < 1e-14);
        assert!(m.convergence_value() > 0.0);
    }
}
