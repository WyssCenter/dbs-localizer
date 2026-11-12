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
// Portions derived from ITK `itkConjugateGradientLineSearchOptimizerv4.hxx` (`AdvanceOneStep`,
// `StartOptimization`: Polak-Ribiere direction with the restart rule),
// `itkGradientDescentLineSearchOptimizerv4.hxx` (`GoldenSectionSearch`, defaults),
// `itkGradientDescentOptimizerv4.hxx` (`ResumeOptimization`: metric evaluation, convergence
// monitoring, best parameters, `EstimateLearningRate`), `itkGradientDescentOptimizerBasev4.hxx`
// (`ModifyGradientByScales`, `StartOptimization`: scale estimation per level),
// `itkRegistrationParameterScalesFromShiftBase.hxx` (`EstimateScales`, `EstimateStepScale`,
// `ComputeMaximumVoxelShift`), `itkRegistrationParameterScalesFromPhysicalShift.hxx`
// (`ComputeSampleShiftsInternal`), `itkRegistrationParameterScalesEstimator.hxx`
// (`SampleVirtualDomainWithCorners`) and from ANTs `itkantsRegistrationHelper.hxx` (the optimizer
// settings of the linear stages: lower limit 0, upper limit 2, epsilon 0.2, learning rate and
// maximum step size from the stage, learning rate estimated once per level). See
// `docs/THIRD_PARTY_PORTS.md`.

//! The optimiser of the linear `antsRegistration` stages: parameter scales from physical shifts and
//! ITK's conjugate-gradient optimiser with a golden-section line search.

use std::sync::atomic::AtomicBool;

use crate::convergence::WindowConvergenceMonitor;
use crate::error::{check_cancel, Result};
use crate::grid::Grid;
use crate::linalg::{vec3_norm, vec3_sub, Vec3};
use crate::transform::LinearMovingTransform;

#[allow(clippy::approx_constant)]
const RESPHI: f64 = 2.0 - 1.618034;

/// What the optimiser needs from a metric on the current level.
///
/// Both methods evaluate at the transform they are given. ITK's sign convention holds:
/// `derivative` is the direction in which the parameters are added to decrease `value`.
pub trait LinearMetric {
    /// The metric value (ITK `GetValue`).
    fn value(&self, t: &LinearMovingTransform) -> Result<f64>;
    /// The metric value and its derivative (ITK `GetValueAndDerivative`).
    fn value_and_derivative(&self, t: &LinearMovingTransform) -> Result<(f64, Vec<f64>)>;
}

/// Settings of one optimiser run (one level of one linear stage).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CgSettings {
    /// `MaximumStepSizeInPhysicalUnits`, also the initial learning rate: the first step moves the
    /// farthest sample point by this many millimetres (`Rigid[0.1]`, `Affine[0.1]`).
    pub max_step_mm: f64,
    /// `LowerLimit` of the line search, times the learning rate.
    pub lower_limit: f64,
    /// `UpperLimit` of the line search, times the learning rate.
    pub upper_limit: f64,
    /// Relative width at which the line search stops.
    pub epsilon: f64,
    /// `MaximumLineSearchIterations`.
    pub max_line_search_iterations: u32,
    /// Iterations of the level.
    pub iterations: u32,
    /// Convergence threshold (stop when the window value is at most this).
    pub convergence_threshold: f64,
    /// Convergence window size.
    pub convergence_window: usize,
    /// `SmallParameterVariation` of the scale estimator.
    pub small_parameter_variation: f64,
}

impl CgSettings {
    /// The `antsRegistration` configuration of a Rigid or Affine stage with the given gradient
    /// step, iteration limit, threshold and window.
    pub fn ants(max_step_mm: f64, iterations: u32, threshold: f64, window: usize) -> Self {
        Self {
            max_step_mm,
            lower_limit: 0.0,
            upper_limit: 2.0,
            epsilon: 0.2,
            max_line_search_iterations: 20,
            iterations,
            convergence_threshold: threshold,
            convergence_window: window,
            small_parameter_variation: 0.01,
        }
    }
}

/// What happened during one optimiser run.
#[derive(Clone, Debug, PartialEq)]
pub struct CgTrace {
    /// Metric value at the start of every iteration that took a step.
    pub values: Vec<f64>,
    /// The metric value on which the convergence monitor stopped the level, which took no step
    /// and so is not in `values`; `None` when the level ran to its iteration limit.
    pub converged_value: Option<f64>,
    /// Iterations that ran (steps taken).
    pub iterations: u32,
    /// Whether the convergence monitor ended the run before the iteration limit.
    pub converged: bool,
    /// Best metric value seen (the transform returned is the one ITK paired with it).
    pub best_value: f64,
    /// Parameter scales estimated at the start of the level.
    pub scales: Vec<f64>,
    /// Learning rate estimated at the first iteration.
    pub initial_learning_rate: f64,
    /// Learning rate after the last line search.
    pub final_learning_rate: f64,
    /// Metric value evaluations spent in line searches.
    pub line_search_evaluations: u64,
}

impl CgTrace {
    /// The metric value at the last iteration that took a step: the value on the last
    /// `DIAGNOSTIC` line ANTs prints for the level. `None` when no iteration stepped.
    pub fn final_value(&self) -> Option<f64> {
        self.values.last().copied()
    }
}

/// The eight corner points of a virtual grid (`SampleVirtualDomainWithCorners`).
pub fn corner_points(grid: &Grid) -> [Vec3; 8] {
    let size = grid.size();
    let mut out = [[0.0; 3]; 8];
    for (i, o) in out.iter_mut().enumerate() {
        let idx = [
            if i & 1 != 0 {
                (size[0] - 1) as f64
            } else {
                0.0
            },
            if i & 2 != 0 {
                (size[1] - 1) as f64
            } else {
                0.0
            },
            if i & 4 != 0 {
                (size[2] - 1) as f64
            } else {
                0.0
            },
        ];
        *o = grid.index_to_point(idx);
    }
    out
}

fn max_voxel_shift(corners: &[Vec3], t: &LinearMovingTransform, delta: &[f64]) -> Result<f64> {
    let mut params = t.optimised.parameters();
    for (p, d) in params.iter_mut().zip(delta) {
        *p += d;
    }
    let moved = t.with_parameters(&params)?;
    let mut max = 0.0f64;
    for c in corners {
        let shift = vec3_norm(vec3_sub(moved.apply(*c), t.apply(*c)));
        max = max.max(shift);
    }
    Ok(max)
}

/// `RegistrationParameterScalesFromPhysicalShift::EstimateScales` for a global linear transform
/// sampled at the corners of the level's virtual grid: `scale_i = (maxShift_i / delta)^2`, a zero
/// shift takes the smallest non-zero shift squared, all zero gives ones.
pub fn estimate_scales(
    corners: &[Vec3],
    t: &LinearMovingTransform,
    small_variation: f64,
) -> Result<Vec<f64>> {
    let n = t.optimised.number_of_parameters();
    let mut scales = vec![0.0f64; n];
    let mut min_non_zero = f64::MAX;
    for i in 0..n {
        let mut delta = vec![0.0; n];
        delta[i] = small_variation;
        let shift = max_voxel_shift(corners, t, &delta)?;
        scales[i] = shift;
        if shift > f64::EPSILON && shift < min_non_zero {
            min_non_zero = shift;
        }
    }
    if min_non_zero == f64::MAX {
        return Ok(vec![1.0; n]);
    }
    for s in &mut scales {
        if *s <= f64::EPSILON {
            *s = min_non_zero * min_non_zero;
        } else {
            *s *= *s;
        }
        *s *= 1.0 / (small_variation * small_variation);
    }
    Ok(scales)
}

/// `EstimateStepScale`: the largest corner shift produced by `step`, extrapolated linearly from
/// a step scaled down to `small_variation` in its largest component.
pub fn estimate_step_scale(
    corners: &[Vec3],
    t: &LinearMovingTransform,
    step: &[f64],
    small_variation: f64,
) -> Result<f64> {
    let max_step = step.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    if max_step <= f64::EPSILON {
        return Ok(0.0);
    }
    let factor = small_variation / max_step;
    let small: Vec<f64> = step.iter().map(|s| s * factor).collect();
    Ok(max_voxel_shift(corners, t, &small)? / factor)
}

fn inner(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

struct LineSearch<'a, M: LinearMetric> {
    metric: &'a M,
    start: &'a LinearMovingTransform,
    direction: &'a [f64],
    settings: &'a CgSettings,
    iterations: u32,
    evaluations: u64,
}

impl<M: LinearMetric> LineSearch<'_, M> {
    fn value_at(&mut self, step: f64) -> Result<f64> {
        self.evaluations += 1;
        let params: Vec<f64> = self
            .start
            .optimised
            .parameters()
            .iter()
            .zip(self.direction)
            .map(|(p, d)| p + step * d)
            .collect();
        self.metric.value(&self.start.with_parameters(&params)?)
    }

    fn golden(&mut self, mut a: f64, mut b: f64, mut c: f64) -> Result<f64> {
        let mut metric_b = f64::MAX;
        loop {
            if self.iterations > self.settings.max_line_search_iterations {
                return Ok((c + a) / 2.0);
            }
            self.iterations += 1;
            let x = if c - b > b - a {
                b + RESPHI * (c - b)
            } else {
                b - RESPHI * (b - a)
            };
            if (c - a).abs() < self.settings.epsilon * (b.abs() + x.abs()) {
                return Ok((c + a) / 2.0);
            }
            let metric_x = self.value_at(x)?;
            if metric_b == f64::MAX {
                metric_b = self.value_at(b)?;
            }
            if metric_x < metric_b {
                if c - b > b - a {
                    a = b;
                    b = x;
                } else {
                    c = b;
                    b = x;
                }
                metric_b = metric_x;
            } else if c - b > b - a {
                c = x;
            } else if metric_x == f64::MAX {
                c = b;
                b = x;
                metric_b = metric_x;
            } else {
                a = x;
            }
        }
    }
}

/// Runs ITK's `ConjugateGradientLineSearchOptimizerv4` for one level from `start` and returns
/// the final transform (the best one, as `ReturnBestParametersAndValue` does) with a trace.
///
/// Per iteration: value and derivative; the convergence monitor; the derivative divided by the
/// scales; at the first iteration `learning_rate = max_step / EstimateStepScale(gradient)`;
/// Polak-Ribiere `gamma` with restart when `gamma < 0` or `gamma > 5`; golden-section search over
/// `[lower, 1, upper] * learning_rate`; parameters moved by `learning_rate * direction`.
/// ITK records the best value with the parameters *after* the step taken from it; so does this.
pub fn optimise_level<M: LinearMetric>(
    metric: &M,
    corners: &[Vec3],
    start: LinearMovingTransform,
    settings: &CgSettings,
    cancel: Option<&AtomicBool>,
) -> Result<(LinearMovingTransform, CgTrace)> {
    let n = start.optimised.number_of_parameters();
    let scales = estimate_scales(corners, &start, settings.small_parameter_variation)?;
    let mut current = start;
    let mut conjugate = vec![0.0f64; n];
    let mut last_gradient = vec![0.0f64; n];
    let mut learning_rate = settings.max_step_mm;
    let mut monitor = WindowConvergenceMonitor::new(settings.convergence_window);
    let mut best_value = f64::MAX;
    let mut best = current;
    let mut trace = CgTrace {
        values: Vec::new(),
        converged_value: None,
        iterations: 0,
        converged: false,
        best_value: f64::MAX,
        scales: scales.clone(),
        initial_learning_rate: learning_rate,
        final_learning_rate: learning_rate,
        line_search_evaluations: 0,
    };
    let mut iteration = 0u32;
    while iteration < settings.iterations {
        check_cancel(cancel)?;
        let (value, mut gradient) = metric.value_and_derivative(&current)?;
        monitor.add_energy_value(value);
        if monitor.convergence_value() <= settings.convergence_threshold {
            trace.converged = true;
            trace.converged_value = Some(value);
            break;
        }
        trace.values.push(value);
        for (g, s) in gradient.iter_mut().zip(&scales) {
            *g *= 1.0 / s;
        }
        if iteration == 0 {
            let step_scale = estimate_step_scale(
                corners,
                &current,
                &gradient,
                settings.small_parameter_variation,
            )?;
            learning_rate = if step_scale <= f64::EPSILON {
                1.0
            } else {
                settings.max_step_mm / step_scale
            };
            trace.initial_learning_rate = learning_rate;
        }
        let mut gamma = 0.0;
        let denom = inner(&last_gradient, &last_gradient);
        if denom > f64::EPSILON {
            let diff: Vec<f64> = gradient
                .iter()
                .zip(&last_gradient)
                .map(|(g, l)| g - l)
                .collect();
            gamma = inner(&diff, &gradient) / denom;
        }
        if !(0.0..=5.0).contains(&gamma) {
            gamma = 0.0;
        }
        last_gradient.clone_from(&gradient);
        for (c, g) in conjugate.iter_mut().zip(&gradient) {
            *c = g + *c * gamma;
        }
        let mut search = LineSearch {
            metric,
            start: &current,
            direction: &conjugate,
            settings,
            iterations: 0,
            evaluations: 0,
        };
        learning_rate = search.golden(
            learning_rate * settings.lower_limit,
            learning_rate,
            learning_rate * settings.upper_limit,
        )?;
        trace.line_search_evaluations += search.evaluations;
        let params: Vec<f64> = current
            .optimised
            .parameters()
            .iter()
            .zip(&conjugate)
            .map(|(p, c)| p + learning_rate * c)
            .collect();
        current = current.with_parameters(&params)?;
        if value < best_value {
            best_value = value;
            best = current;
        }
        iteration += 1;
    }
    trace.iterations = iteration;
    trace.best_value = best_value;
    trace.final_learning_rate = learning_rate;
    Ok((best, trace))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;
    use crate::transform::{AffineTransform, ParametricTransform};

    struct Quadratic {
        target: [f64; 6],
        weights: [f64; 6],
    }

    impl LinearMetric for Quadratic {
        fn value(&self, t: &LinearMovingTransform) -> Result<f64> {
            let p = t.optimised.parameters();
            Ok((0..6)
                .map(|i| self.weights[i] * (p[i] - self.target[i]).powi(2))
                .sum())
        }
        fn value_and_derivative(&self, t: &LinearMovingTransform) -> Result<(f64, Vec<f64>)> {
            let p = t.optimised.parameters();
            let d = (0..6)
                .map(|i| -2.0 * self.weights[i] * (p[i] - self.target[i]))
                .collect();
            Ok((self.value(t)?, d))
        }
    }

    fn rigid(centre: Vec3) -> LinearMovingTransform {
        LinearMovingTransform {
            outer: AffineTransform::identity(),
            optimised: ParametricTransform::euler3d([0.0; 6], centre),
        }
    }

    fn cube_corners(half: f64) -> Vec<Vec3> {
        let mut v = Vec::new();
        for x in [-half, half] {
            for y in [-half, half] {
                for z in [-half, half] {
                    v.push([x, y, z]);
                }
            }
        }
        v
    }

    #[test]
    fn rigid_scales_are_squared_corner_shifts_per_unit_parameter() {
        let corners = cube_corners(50.0);
        let t = rigid([0.0; 3]);
        let s = estimate_scales(&corners, &t, 0.01).unwrap();
        for translation in &s[3..] {
            assert!((translation - 1.0).abs() < 1e-9);
        }
        let want = 50.0f64.hypot(50.0).powi(2);
        for rotation in &s[..3] {
            assert!((rotation - want).abs() / want < 1e-3, "{rotation} {want}");
        }
    }

    #[test]
    fn affine_scales_follow_the_corner_distance_from_the_centre() {
        let corners = cube_corners(40.0);
        let t = LinearMovingTransform {
            outer: AffineTransform::identity(),
            optimised: ParametricTransform::affine(&AffineTransform::new(
                IDENTITY3, [0.0; 3], [0.0; 3],
            )),
        };
        let s = estimate_scales(&corners, &t, 0.01).unwrap();
        assert_eq!(s.len(), 12);
        for m in &s[..9] {
            assert!((m - 40.0f64.powi(2)).abs() < 1e-6);
        }
        for tr in &s[9..] {
            assert!((tr - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn step_scale_is_the_corner_shift_of_the_step() {
        let corners = cube_corners(50.0);
        let t = rigid([0.0; 3]);
        let step = [0.0, 0.0, 0.0, 3.0, 4.0, 0.0];
        let s = estimate_step_scale(&corners, &t, &step, 0.01).unwrap();
        assert!((s - 5.0).abs() < 1e-9);
        assert_eq!(
            estimate_step_scale(&corners, &t, &[0.0; 6], 0.01).unwrap(),
            0.0
        );
    }

    #[test]
    fn conjugate_gradient_with_line_search_minimises_a_quadratic() {
        let metric = Quadratic {
            target: [0.0, 0.0, 0.0, 3.0, -2.0, 1.0],
            weights: [1.0; 6],
        };
        let corners = cube_corners(20.0);
        let settings = CgSettings::ants(0.1, 200, 1e-12, 10);
        let (t, trace) =
            optimise_level(&metric, &corners, rigid([0.0; 3]), &settings, None).unwrap();
        let p = t.optimised.parameters();
        assert!((p[3] - 3.0).abs() < 0.05, "{p:?}");
        assert!((p[4] + 2.0).abs() < 0.05, "{p:?}");
        assert!((p[5] - 1.0).abs() < 0.05, "{p:?}");
        assert!(trace.best_value < 0.01);
        assert!(trace.line_search_evaluations > 0);
        assert!(trace.initial_learning_rate > 0.0);
    }

    #[test]
    fn the_monitor_stops_a_converged_level_and_zero_iterations_return_the_start() {
        let metric = Quadratic {
            target: [0.0; 6],
            weights: [1.0; 6],
        };
        let corners = cube_corners(20.0);
        let settings = CgSettings::ants(0.1, 500, 1e-6, 10);
        let (_, trace) =
            optimise_level(&metric, &corners, rigid([0.0; 3]), &settings, None).unwrap();
        assert!(trace.converged);
        assert!(trace.iterations < 500);
        assert_eq!(trace.values.len(), trace.iterations as usize);
        assert!(trace.converged_value.is_some());
        assert_eq!(trace.final_value(), trace.values.last().copied());
        let none = CgSettings::ants(0.1, 0, 1e-6, 10);
        let (t, trace) = optimise_level(&metric, &corners, rigid([0.0; 3]), &none, None).unwrap();
        assert_eq!(trace.iterations, 0);
        assert_eq!(t, rigid([0.0; 3]));
    }

    struct CancelAfter<'a> {
        inner: Quadratic,
        cancel: &'a AtomicBool,
        evaluations: std::cell::Cell<u32>,
        limit: u32,
    }

    impl LinearMetric for CancelAfter<'_> {
        fn value(&self, t: &LinearMovingTransform) -> Result<f64> {
            self.inner.value(t)
        }
        fn value_and_derivative(&self, t: &LinearMovingTransform) -> Result<(f64, Vec<f64>)> {
            let n = self.evaluations.get() + 1;
            self.evaluations.set(n);
            if n >= self.limit {
                self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            self.inner.value_and_derivative(t)
        }
    }

    #[test]
    fn a_cancel_flag_stops_the_optimiser_at_the_next_iteration() {
        use crate::error::RegError;
        let corners = cube_corners(20.0);
        let settings = CgSettings::ants(0.1, 200, 1e-12, 10);
        let quadratic = || Quadratic {
            target: [0.0, 0.0, 0.0, 3.0, -2.0, 1.0],
            weights: [1.0; 6],
        };
        let set = AtomicBool::new(true);
        let e = optimise_level(
            &quadratic(),
            &corners,
            rigid([0.0; 3]),
            &settings,
            Some(&set),
        )
        .unwrap_err();
        assert!(matches!(e, RegError::Cancelled), "{e}");
        let flag = AtomicBool::new(false);
        let metric = CancelAfter {
            inner: quadratic(),
            cancel: &flag,
            evaluations: std::cell::Cell::new(0),
            limit: 3,
        };
        let e =
            optimise_level(&metric, &corners, rigid([0.0; 3]), &settings, Some(&flag)).unwrap_err();
        assert!(matches!(e, RegError::Cancelled), "{e}");
        assert_eq!(metric.evaluations.get(), 3);
        let unset = AtomicBool::new(false);
        assert!(optimise_level(
            &quadratic(),
            &corners,
            rigid([0.0; 3]),
            &settings,
            Some(&unset)
        )
        .is_ok());
    }
}
