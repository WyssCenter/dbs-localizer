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
// Portions derived from ITK `itkSyNImageRegistrationMethod.hxx` (`InitializeRegistrationAtEachLevel`,
// `StartOptimization`, `ComputeUpdateField`, `ComputeMetricGradientField` with
// `DownsampleImagesForMetricDerivatives`, `GenerateData`), `itkObjectToObjectMultiMetricv4.hxx`
// (`GetValueAndDerivative`, `GetWeightedValue`, weight normalisation), `itkImageRegistrationMethodv4.hxx`
// (`InitializeRegistrationAtEachLevel`: shrunk virtual domain, per-level smoothing, iteration state)
// and from ANTs `itkantsRegistrationHelper.hxx` (the SyN stage set-up). See
// `docs/THIRD_PARTY_PORTS.md`.

//! ITK's symmetric normalisation (`SyNImageRegistrationMethod`) as `antsRegistration` drives it,
//! written against a [`Backend`].
//!
//! Four fields live on the virtual grid of the current level: fixed-to-middle `F`, moving-to-middle
//! `M` and their inverses. One iteration:
//!
//! 1. every channel's smoothed full-resolution fixed image is resampled onto the level grid
//!    through `F^-1`, and the moving image through `affine(M^-1(x))`;
//! 2. the ANTs CC metric runs on those two images (identity transforms, so its derivative is a
//!    per-voxel image-space gradient), once with the fixed-side image as "fixed" and once with the
//!    roles swapped; several channels combine as ITK's multi-metric
//!    `(sum |d_j| / J) sum w_j d_j / |d_j|`; the value is the first metric's;
//! 3. each gradient is Gaussian-smoothed (update variance) and scaled so its largest voxel step is
//!    the gradient step;
//! 4. `F <- F + u_F(x + F(x))` (likewise `M`), optionally smoothed with the total-field variance;
//! 5. both inverses are re-estimated from the previous inverse and each field is replaced by the
//!    inverse of its inverse: `Finv = invert(F_tmp, init = Finv)`, `F = invert(Finv, init = F_tmp)`;
//! 6. the energy `0.5 * (value_a + value_b)` enters the window convergence monitor.
//!
//! The result is `SyN = Minv o F` composed with the initial affine into a [`CompositePair`].

use std::borrow::Cow;
use std::sync::atomic::AtomicBool;

use crate::backend::Backend;
use crate::convergence::WindowConvergenceMonitor;
use crate::error::{check_cancel, invalid, Result};
use crate::field_ops::{InvertParams, InvertStats};
use crate::grid::Grid;
use crate::linear::SmoothCache;
use crate::pyramid::{
    level_virtual_grid, shrink_factors_per_dimension, smooth_for_level_cow, SigmaUnits,
};
use crate::recipe::SynRecipe;
use crate::transform::{AffineTransform, CompositePair, DisplacementField};
use crate::volume::Volume;

/// One metric channel: preprocessed full-resolution fixed and moving images (ANTs windowed them
/// to `[0, 1]` before the stage). The fixed images of all channels must share a grid.
#[derive(Clone, Debug)]
pub struct SynChannel {
    /// The fixed image.
    pub fixed: Volume<f64>,
    /// The moving image.
    pub moving: Volume<f64>,
}

/// Options outside the `antsRegistration` command line: `cancel`, polled at the start of every
/// level and every iteration (and by the linear stages of [`crate::register::register_full`]);
/// once it is set the run ends with [`crate::error::RegError::Cancelled`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SynOptions<'a> {
    pub cancel: Option<&'a AtomicBool>,
}

/// What one iteration measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SynIteration {
    /// Energy given to the monitor: `0.5 * (a + b)` of the first-metric values.
    pub energy: f64,
    /// `0.5 * (a + b)` of the first-metric values (what ITK uses).
    pub energy_first_metric: f64,
    /// `0.5 * (a + b)` of the weighted values.
    pub energy_weighted: f64,
    /// Monitor value after this iteration (`f64::MAX` until the window is full).
    pub convergence_value: f64,
    /// Scale applied to the fixed-side update (gradient step over its largest voxel step).
    pub fixed_update_scale: f64,
    /// Scale applied to the moving-side update.
    pub moving_update_scale: f64,
    /// Inversion statistics in call order: `Finv`, `F`, `Minv`, `M`.
    pub inversions: [InvertStats; 4],
}

/// The trace of one level.
#[derive(Clone, Debug, PartialEq)]
pub struct SynLevelTrace {
    /// Level index.
    pub level: usize,
    /// Shrink factors applied to the fixed grid.
    pub shrink_factors: [u32; 3],
    /// Voxels of the level grid.
    pub grid_size: [usize; 3],
    /// One entry per executed iteration.
    pub iterations: Vec<SynIteration>,
    /// Whether the monitor stopped the level before its iteration limit.
    pub converged: bool,
}

/// Output of [`run_syn`].
#[derive(Clone, Debug)]
pub struct SynResult {
    /// `Composite = [Affine, Warp]`, `InverseComposite = [InverseWarp, Affine^-1]`.
    pub pair: CompositePair,
    /// `SyN(x) = F(x) + Minv(x + F(x))` on the last level grid, `f32` like `1Warp.nii.gz`.
    pub warp: DisplacementField,
    /// `SyN^-1(x) = M(x) + Finv(x + M(x))` on the last level grid.
    pub inverse_warp: DisplacementField,
    /// Per-level traces.
    pub levels: Vec<SynLevelTrace>,
}

struct State<B: Backend> {
    f2m: B::Field,
    f2m_inv: B::Field,
    m2m: B::Field,
    m2m_inv: B::Field,
}

struct SideUpdate<B: Backend> {
    update: B::Field,
    scale: f64,
    first_value: f64,
    weighted_value: f64,
}

struct SideAccumulator<B: Backend> {
    values: Vec<f64>,
    gradient: Option<B::Field>,
    total_magnitude: f64,
}

impl<B: Backend> SideAccumulator<B> {
    fn new(channels: usize) -> Self {
        Self {
            values: Vec::with_capacity(channels),
            gradient: None,
            total_magnitude: 0.0,
        }
    }

    fn add(
        &mut self,
        backend: &B,
        weight: f64,
        value: f64,
        derivative: B::Field,
        magnitude: Option<f64>,
    ) -> Result<()> {
        self.values.push(value);
        let Some(magnitude) = magnitude else {
            self.gradient = Some(derivative);
            return Ok(());
        };
        let acc = match self.gradient.as_mut() {
            Some(acc) => acc,
            None => {
                let grid = backend.field_grid(&derivative);
                self.gradient.insert(backend.zero_field(&grid)?)
            }
        };
        if !magnitude.is_finite() {
            return Err(invalid("syn", "the metric gradient is not finite"));
        }
        self.total_magnitude += magnitude;
        if magnitude > f64::EPSILON {
            backend.axpy(acc, weight / magnitude, &derivative)?;
        }
        Ok(())
    }

    fn smoothed(
        self,
        backend: &B,
        recipe: &SynRecipe,
        normalised: &[f64],
    ) -> Result<SmoothedSide<B>> {
        let channels = self.values.len();
        let first_value = self.values[0];
        let mut gradient = self
            .gradient
            .ok_or_else(|| invalid("syn", "no channel produced a gradient"))?;
        let weighted_value = if channels == 1 {
            first_value
        } else {
            backend.scale(&mut gradient, self.total_magnitude / channels as f64)?;
            self.values.iter().zip(normalised).map(|(v, w)| v * w).sum()
        };
        let gradient = backend.smooth_field(gradient, recipe.update_field_variance)?;
        Ok(SmoothedSide {
            gradient,
            first_value,
            weighted_value,
        })
    }
}

struct SmoothedSide<B: Backend> {
    gradient: B::Field,
    first_value: f64,
    weighted_value: f64,
}

impl<B: Backend> SmoothedSide<B> {
    fn finish(self, backend: &B, recipe: &SynRecipe, max_norm: f64) -> Result<SideUpdate<B>> {
        let mut scale = recipe.gradient_step;
        if max_norm > 0.0 {
            scale /= max_norm;
        }
        let mut gradient = self.gradient;
        backend.scale(&mut gradient, scale)?;
        Ok(SideUpdate {
            update: gradient,
            scale,
            first_value: self.first_value,
            weighted_value: self.weighted_value,
        })
    }
}

fn side_updates<B: Backend>(
    backend: &B,
    recipe: &SynRecipe,
    fixed_mid: &[B::Image],
    moving_mid: &[B::Image],
) -> Result<(SideUpdate<B>, SideUpdate<B>)> {
    let channels = fixed_mid.len();
    let sum: f64 = recipe.channel_weights.iter().sum();
    let normalised: Vec<f64> = recipe.channel_weights.iter().map(|w| w / sum).collect();
    let mut fixed_side = SideAccumulator::<B>::new(channels);
    let mut moving_side = SideAccumulator::<B>::new(channels);
    for c in 0..channels {
        let (((fixed_value, fixed_derivative), (moving_value, moving_derivative)), norms) =
            if channels == 1 {
                let pair = backend.cc_value_derivative_pair(
                    &fixed_mid[c],
                    &moving_mid[c],
                    recipe.cc_radius,
                )?;
                (pair, None)
            } else {
                let (pair, norms) = backend.cc_value_derivative_pair_with_norms(
                    &fixed_mid[c],
                    &moving_mid[c],
                    recipe.cc_radius,
                )?;
                (pair, Some(norms))
            };
        fixed_side.add(
            backend,
            normalised[c],
            fixed_value,
            fixed_derivative,
            norms.map(|n| n[0]),
        )?;
        moving_side.add(
            backend,
            normalised[c],
            moving_value,
            moving_derivative,
            norms.map(|n| n[1]),
        )?;
    }
    let fixed_side = fixed_side.smoothed(backend, recipe, &normalised)?;
    let moving_side = moving_side.smoothed(backend, recipe, &normalised)?;
    let (fixed_max, moving_max) =
        backend.max_scaled_norm_pair(&fixed_side.gradient, &moving_side.gradient)?;
    if !(fixed_max.is_finite() && moving_max.is_finite()) {
        return Err(invalid("syn", "the metric gradient is not finite"));
    }
    let fixed_update = fixed_side.finish(backend, recipe, fixed_max)?;
    let moving_update = moving_side.finish(backend, recipe, moving_max)?;
    Ok((fixed_update, moving_update))
}

fn update_total_field<B: Backend>(
    backend: &B,
    recipe: &SynRecipe,
    update: B::Field,
    field: B::Field,
    inverse: B::Field,
) -> Result<(B::Field, B::Field, InvertStats, InvertStats)> {
    let composed = backend.compose(&update, &field)?;
    drop(update);
    drop(field);
    let tmp = backend.smooth_field(composed, recipe.total_field_variance)?;
    let ((new_inverse, stats_inverse), (new_field, stats_field)) =
        backend.invert_pair(tmp, inverse, &InvertParams::SYN)?;
    if [stats_inverse, stats_field]
        .iter()
        .any(|s| !(s.mean_error_norm.is_finite() && s.max_error_norm.is_finite()))
    {
        return Err(invalid("syn", "the displacement field is not finite"));
    }
    Ok((new_field, new_inverse, stats_inverse, stats_field))
}

fn upload_smoothed<B: Backend>(
    backend: &B,
    vol: &Volume<f64>,
    sigma: f64,
    units: SigmaUnits,
) -> Result<B::Image> {
    match smooth_for_level_cow(vol, sigma, units)? {
        Cow::Borrowed(v) => backend.upload_image(v),
        Cow::Owned(v) => backend.upload_image_owned(v),
    }
}

/// Runs the SyN stage described by `recipe`.
///
/// `initial` is the linear transform of the earlier stages (maps fixed points to moving points);
/// it enters the moving side of every metric evaluation and the final composite. Level
/// transitions resample all four fields onto the next virtual grid (linear, zero outside,
/// values unscaled).
pub fn run_syn<B: Backend>(
    backend: &B,
    recipe: &SynRecipe,
    channels: &[SynChannel],
    initial: &AffineTransform,
    options: &SynOptions<'_>,
) -> Result<SynResult> {
    run_syn_cached(backend, recipe, channels, initial, options, None)
}

/// [`run_syn`] taking the smoothed copies of the first channel from `cache` (filled by the
/// linear stages on the same images, [`crate::linear::run_linear_chain_cached`]) instead of
/// smoothing them again; every pair it uses is removed from the cache.
pub fn run_syn_cached<B: Backend>(
    backend: &B,
    recipe: &SynRecipe,
    channels: &[SynChannel],
    initial: &AffineTransform,
    options: &SynOptions<'_>,
    cache: Option<&mut SmoothCache>,
) -> Result<SynResult> {
    let (warp, inverse_warp, levels) =
        syn_fields(backend, recipe, channels, initial, cache, options.cancel)?;
    let pair = CompositePair::new(initial, warp.clone(), inverse_warp.clone())?;
    Ok(SynResult {
        pair,
        warp,
        inverse_warp,
        levels,
    })
}

pub(crate) fn run_syn_pair_cached<B: Backend>(
    backend: &B,
    recipe: &SynRecipe,
    channels: &[SynChannel],
    initial: &AffineTransform,
    options: &SynOptions<'_>,
    cache: Option<&mut SmoothCache>,
) -> Result<(CompositePair, Vec<SynLevelTrace>)> {
    let (warp, inverse_warp, levels) =
        syn_fields(backend, recipe, channels, initial, cache, options.cancel)?;
    Ok((CompositePair::new(initial, warp, inverse_warp)?, levels))
}

fn syn_fields<B: Backend>(
    backend: &B,
    recipe: &SynRecipe,
    channels: &[SynChannel],
    initial: &AffineTransform,
    mut cache: Option<&mut SmoothCache>,
    cancel: Option<&AtomicBool>,
) -> Result<(DisplacementField, DisplacementField, Vec<SynLevelTrace>)> {
    recipe.validate()?;
    if channels.is_empty() {
        return Err(invalid("syn", "no channels"));
    }
    if channels.len() != recipe.channel_weights.len() {
        return Err(invalid(
            "syn",
            "the number of channels differs from the number of channel weights",
        ));
    }
    let fixed_grid: Grid = *channels[0].fixed.grid();
    for c in channels {
        if !c.fixed.grid().approx_eq(&fixed_grid, 1e-9) {
            return Err(invalid(
                "syn",
                "fixed images of the channels differ in grid",
            ));
        }
    }
    let mut state: Option<State<B>> = None;
    let mut levels = Vec::with_capacity(recipe.levels());
    for level in 0..recipe.levels() {
        check_cancel(cancel)?;
        let factors =
            shrink_factors_per_dimension(recipe.shrink_factors[level], fixed_grid.spacing());
        let grid = level_virtual_grid(&fixed_grid, factors)?;
        let sigma = recipe.smoothing_sigmas[level];
        let mut fixed_smooth = Vec::with_capacity(channels.len());
        let mut moving_smooth = Vec::with_capacity(channels.len());
        for (index, c) in channels.iter().enumerate() {
            let cached = match cache.as_deref_mut() {
                Some(cache) if index == 0 && sigma > 0.0 => {
                    cache.take(&c.fixed, &c.moving, sigma, recipe.sigma_units)
                }
                _ => None,
            };
            match cached {
                Some((f, m)) => {
                    fixed_smooth.push(backend.upload_image_owned(f)?);
                    moving_smooth.push(backend.upload_image_owned(m)?);
                }
                None => {
                    fixed_smooth.push(upload_smoothed(
                        backend,
                        &c.fixed,
                        sigma,
                        recipe.sigma_units,
                    )?);
                    moving_smooth.push(upload_smoothed(
                        backend,
                        &c.moving,
                        sigma,
                        recipe.sigma_units,
                    )?);
                }
            }
        }
        let mut st = match state.take() {
            None => State::<B> {
                f2m: backend.zero_field(&grid)?,
                f2m_inv: backend.zero_field(&grid)?,
                m2m: backend.zero_field(&grid)?,
                m2m_inv: backend.zero_field(&grid)?,
            },
            Some(prev) => {
                let State {
                    f2m,
                    f2m_inv,
                    m2m,
                    m2m_inv,
                } = prev;
                let next_f2m = backend.resample_field(&f2m, &grid)?;
                drop(f2m);
                let next_f2m_inv = backend.resample_field(&f2m_inv, &grid)?;
                drop(f2m_inv);
                let next_m2m = backend.resample_field(&m2m, &grid)?;
                drop(m2m);
                let next_m2m_inv = backend.resample_field(&m2m_inv, &grid)?;
                drop(m2m_inv);
                State::<B> {
                    f2m: next_f2m,
                    f2m_inv: next_f2m_inv,
                    m2m: next_m2m,
                    m2m_inv: next_m2m_inv,
                }
            }
        };
        let mut monitor = WindowConvergenceMonitor::new(recipe.convergence_window);
        let mut converged = false;
        let mut trace = Vec::new();
        let limit = recipe.iterations[level];
        let mut current = 0u32;
        while current < limit && !converged {
            check_cancel(cancel)?;
            current += 1;
            let mut fixed_mid = Vec::with_capacity(channels.len());
            let mut moving_mid = Vec::with_capacity(channels.len());
            for c in 0..channels.len() {
                fixed_mid.push(backend.warp_through_field(&fixed_smooth[c], &st.f2m_inv, None)?);
                moving_mid.push(backend.warp_through_field(
                    &moving_smooth[c],
                    &st.m2m_inv,
                    Some(initial),
                )?);
            }
            let (fixed_side, moving_side) = side_updates(backend, recipe, &fixed_mid, &moving_mid)?;
            let State {
                f2m,
                f2m_inv,
                m2m,
                m2m_inv,
            } = st;
            let (f2m, f2m_inv, inv_f_inv, inv_f) =
                update_total_field(backend, recipe, fixed_side.update, f2m, f2m_inv)?;
            let (m2m, m2m_inv, inv_m_inv, inv_m) =
                update_total_field(backend, recipe, moving_side.update, m2m, m2m_inv)?;
            st = State {
                f2m,
                f2m_inv,
                m2m,
                m2m_inv,
            };
            let energy_first = 0.5 * (fixed_side.first_value + moving_side.first_value);
            let energy_weighted = 0.5 * (fixed_side.weighted_value + moving_side.weighted_value);
            let energy = energy_first;
            monitor.add_energy_value(energy);
            let convergence_value = monitor.convergence_value();
            if convergence_value < recipe.convergence_threshold {
                converged = true;
            }
            trace.push(SynIteration {
                energy,
                energy_first_metric: energy_first,
                energy_weighted,
                convergence_value,
                fixed_update_scale: fixed_side.scale,
                moving_update_scale: moving_side.scale,
                inversions: [inv_f_inv, inv_f, inv_m_inv, inv_m],
            });
        }
        levels.push(SynLevelTrace {
            level,
            shrink_factors: factors,
            grid_size: grid.size(),
            iterations: trace,
            converged,
        });
        state = Some(st);
    }
    let st = state.ok_or_else(|| invalid("syn", "no levels were run"))?;
    let forward = backend.compose(&st.m2m_inv, &st.f2m)?;
    let inverse = backend.compose(&st.f2m_inv, &st.m2m)?;
    let warp = backend.download_field(&forward)?.to_displacement_field()?;
    let inverse_warp = backend.download_field(&inverse)?.to_displacement_field()?;
    Ok((warp, inverse_warp, levels))
}
