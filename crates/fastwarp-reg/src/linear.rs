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
// Portions derived from ITK `itkImageRegistrationMethodv4.hxx` (`GenerateData`,
// `InitializeRegistrationAtEachLevel`: shrunk virtual domain, per-level smoothing of the
// full-resolution images, `SetMetricSamplePoints`, `InitializeCenterOfLinearOutputTransform`,
// composite of the earlier transforms with the optimised one applied first) and from ANTs
// `itkantsRegistrationHelper.hxx` (`AddLinearTransformToCompositeTransform`,
// `CollapseLinearTransforms`: collapse of `[initial, rigid, affine]` into one affine transform
// whose centre is that of the last matrix-offset transform). See `docs/THIRD_PARTY_PORTS.md`.

//! The linear stages of an `antsRegistration` run (centre-of-mass initialisation, `Rigid`,
//! `Affine`, both with Mattes mutual information) over an engine that evaluates the metric.

use std::borrow::Cow;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use rayon::prelude::*;

use crate::error::{check_cancel, invalid, Result};
use crate::grid::Grid;
use crate::init::{initial_transform, InitFeature};
use crate::interp::linear_at_point;
use crate::linalg::{mat3_mul_vec, vec3_add, vec3_sub, Vec3};
use crate::mask::MaskVolume;
use crate::metric::mi::{MattesMi, MiBinning, MiInputs};
use crate::optimizer::{corner_points, optimise_level, CgSettings, CgTrace, LinearMetric};
use crate::pyramid::{
    level_virtual_grid, shrink_factors_per_dimension, smooth_for_level, SigmaUnits,
};
use crate::sampling::{metric_sample_points, SamplingSpec, SamplingStrategy, SeedSequence};
use crate::transform::{
    AffineTransform, LinearMovingTransform, ParametricTransform, TransformChain,
};
use crate::volume::Volume;

/// `--random-seed` of the cohort recipes.
pub const COHORT_RANDOM_SEED: u32 = 20_260_910;
/// Histogram bins of `MI[...,1,32,Regular,0.25]`.
pub const COHORT_MI_BINS: usize = 32;
/// Sampling percentage of `MI[...,1,32,Regular,0.25]`.
pub const COHORT_MI_SAMPLING: f32 = 0.25;

/// Kind of linear transform a stage optimises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearKind {
    /// `Rigid[step]`: `Euler3DTransform`, six parameters.
    Rigid,
    /// `Affine[step]`: twelve parameters.
    Affine,
}

/// One `Rigid` or `Affine` stage with a Mattes MI metric as `antsRegistration` runs it.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearRecipe {
    /// Transform kind.
    pub kind: LinearKind,
    /// `Rigid[step]` / `Affine[step]` gradient step (maximum step in millimetres).
    pub gradient_step: f64,
    /// `--convergence [iterations,threshold,window]`: iterations per level.
    pub iterations: Vec<u32>,
    /// Convergence threshold.
    pub convergence_threshold: f64,
    /// Convergence window size.
    pub convergence_window: usize,
    /// `--shrink-factors`.
    pub shrink_factors: Vec<u32>,
    /// `--smoothing-sigmas`.
    pub smoothing_sigmas: Vec<f64>,
    /// Unit of the sigmas.
    pub sigma_units: SigmaUnits,
    /// Histogram bins of `MI[...]`.
    pub bins: usize,
    /// Sampling percentage of `MI[...]` (regular sampling).
    pub sampling_percentage: f32,
    /// `--random-seed`.
    pub seed: u32,
    /// Fixed-image mask (`--masks [fixed,NULL]`), evaluated in physical space during sample
    /// generation and by the metric. `None` samples the whole virtual domain.
    pub mask: Option<MaskVolume>,
    /// Keep the smoothed image pairs of the earlier levels in the [`SmoothCache`] so a chained
    /// stage reuses them. A single CT → anchor stage turns this off to bound host memory: a
    /// 512×512×300 CT is 78.6 M voxels (≈ 0.63 GB per `f64` copy). With the cache on, the retained
    /// pairs of a `3x2x1x0vox` stage reach ≈ 4 GB; with it off the pair of a level is dropped after
    /// that level and the two channels are smoothed sequentially, so the peak is the two raw
    /// volumes plus one smoothed pair plus one channel's three working copies. Measured on such a
    /// CT in release (`host_peak`), peak working set 3.52 GiB, under the 4 GB budget.
    pub cache_smoothed_images: bool,
}

impl LinearRecipe {
    fn cohort(kind: LinearKind) -> Self {
        Self {
            kind,
            gradient_step: 0.1,
            iterations: vec![1000, 500, 250, 100],
            convergence_threshold: 1e-6,
            convergence_window: 10,
            shrink_factors: vec![8, 4, 2, 1],
            smoothing_sigmas: vec![3.0, 2.0, 1.0, 0.0],
            sigma_units: SigmaUnits::Voxels,
            bins: COHORT_MI_BINS,
            sampling_percentage: COHORT_MI_SAMPLING,
            seed: COHORT_RANDOM_SEED,
            mask: None,
            cache_smoothed_images: true,
        }
    }

    /// The same recipe with a fixed-image mask.
    pub fn with_mask(mut self, mask: MaskVolume) -> Self {
        self.mask = Some(mask);
        self
    }

    /// The cohort `Rigid[0.1]` stage: `MI[...,1,32,Regular,0.25]`, `[1000x500x250x100,1e-6,10]`,
    /// `8x4x2x1`, `3x2x1x0vox`.
    pub fn cohort_rigid() -> Self {
        Self::cohort(LinearKind::Rigid)
    }

    /// The cohort `Affine[0.1]` stage (same metric and schedule).
    pub fn cohort_affine() -> Self {
        Self::cohort(LinearKind::Affine)
    }

    /// The argument strings of the stage as `antsRegistration` takes them, from `--transform`
    /// through `--smoothing-sigmas`; `fixed` and `moving` are the image paths of the `MI` metric.
    pub fn to_ants_args(&self, fixed: &str, moving: &str) -> Vec<String> {
        fn join_x<T: std::fmt::Display>(values: &[T]) -> String {
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("x")
        }
        let kind = match self.kind {
            LinearKind::Rigid => "Rigid",
            LinearKind::Affine => "Affine",
        };
        let unit = match self.sigma_units {
            SigmaUnits::Voxels => "vox",
            SigmaUnits::Millimetres => "mm",
        };
        vec![
            "--transform".to_string(),
            format!("{kind}[{}]", self.gradient_step),
            "--metric".to_string(),
            format!(
                "MI[{fixed},{moving},1,{},Regular,{}]",
                self.bins, self.sampling_percentage
            ),
            "--convergence".to_string(),
            format!(
                "[{},{:e},{}]",
                join_x(&self.iterations),
                self.convergence_threshold,
                self.convergence_window
            ),
            "--shrink-factors".to_string(),
            join_x(&self.shrink_factors),
            "--smoothing-sigmas".to_string(),
            format!("{}{unit}", join_x(&self.smoothing_sigmas)),
        ]
    }

    /// Number of resolution levels.
    pub fn levels(&self) -> usize {
        self.iterations.len()
    }

    /// Checks that the per-level vectors agree and every value is usable.
    pub fn validate(&self) -> Result<()> {
        let n = self.iterations.len();
        if n == 0
            || self.shrink_factors.len() != n
            || self.smoothing_sigmas.len() != n
            || self.shrink_factors.contains(&0)
        {
            return Err(invalid(
                "linear recipe",
                "iterations, shrink factors and smoothing sigmas must have one entry per level",
            ));
        }
        if self.convergence_window < 2 || self.bins <= 4 {
            return Err(invalid("linear recipe", "window below 2 or at most 4 bins"));
        }
        if !(self.sampling_percentage > 0.0 && self.sampling_percentage <= 1.0) {
            return Err(invalid(
                "linear recipe",
                "sampling percentage outside (0, 1]",
            ));
        }
        if !(self.gradient_step.is_finite() && self.gradient_step > 0.0) {
            return Err(invalid("linear recipe", "gradient step must be positive"));
        }
        if self
            .smoothing_sigmas
            .iter()
            .any(|v| !(v.is_finite() && *v >= 0.0))
        {
            return Err(invalid(
                "linear recipe",
                "smoothing sigmas must be finite and non-negative",
            ));
        }
        if !self.convergence_threshold.is_finite() {
            return Err(invalid(
                "linear recipe",
                "convergence threshold must be finite",
            ));
        }
        Ok(())
    }
}

/// The sample set of one level with everything the metric needs from the fixed image: points
/// whose fixed point lies inside the fixed buffer, their fixed-axis bin, and the binning.
#[derive(Clone, Debug)]
pub struct PreparedSamples {
    /// Sample points (virtual domain, LPS mm).
    pub points: Vec<Vec3>,
    /// Fixed-axis histogram bin of each point.
    pub fixed_bin: Vec<u32>,
    /// Intensity ranges and bin geometry of the level.
    pub binning: MiBinning,
}

/// ITK's metric initialisation for a level: fixed range over the samples, moving range over the
/// whole moving image, and the fixed bin of every sample inside the fixed buffer.
pub fn prepare_samples(
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    samples: &[Vec3],
    bins: usize,
) -> Result<PreparedSamples> {
    let binning = MiBinning::new(fixed, moving, samples, &TransformChain::new(), bins)?;
    let kept: Vec<Option<u32>> = samples
        .par_iter()
        .map(|s| {
            linear_at_point(fixed, *s)
                .filter(|v| v.is_finite())
                .map(|v| binning.fixed_bin(v) as u32)
        })
        .collect();
    let mut points = Vec::with_capacity(samples.len());
    let mut fixed_bin = Vec::with_capacity(samples.len());
    for (s, b) in samples.iter().zip(kept) {
        if let Some(b) = b {
            points.push(*s);
            fixed_bin.push(b);
        }
    }
    Ok(PreparedSamples {
        points,
        fixed_bin,
        binning,
    })
}

/// Evaluates the metric of a level. Implemented by the CPU reference ([`CpuLinearEngine`]) and by
/// the GPU backend.
pub trait LinearEngine: Sync {
    /// The metric of one prepared level.
    type Level<'a>: LinearMetric
    where
        Self: 'a;

    /// A short name for records.
    fn name(&self) -> String;

    /// Builds the metric of a level from its smoothed images and prepared samples.
    fn prepare_level<'a>(
        &'a self,
        fixed: Volume<f64>,
        moving: Volume<f64>,
        samples: PreparedSamples,
    ) -> Result<Self::Level<'a>>;

    fn prepare_level_borrowed<'a>(
        &'a self,
        fixed: &'a Volume<f64>,
        moving: &'a Volume<f64>,
        samples: PreparedSamples,
    ) -> Result<Self::Level<'a>> {
        self.prepare_level(fixed.clone(), moving.clone(), samples)
    }
}

/// The `f64` CPU reference engine: [`MattesMi`] on the sampled points.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuLinearEngine;

/// The metric of one level on the CPU.
pub struct CpuLevel<'a> {
    fixed: Cow<'a, Volume<f64>>,
    moving: Cow<'a, Volume<f64>>,
    samples: PreparedSamples,
    chain: TransformChain,
}

impl LinearMetric for CpuLevel<'_> {
    fn value(&self, t: &LinearMovingTransform) -> Result<f64> {
        Ok(self.evaluate(t, false)?.0)
    }

    fn value_and_derivative(&self, t: &LinearMovingTransform) -> Result<(f64, Vec<f64>)> {
        self.evaluate(t, true)
    }
}

impl CpuLevel<'_> {
    fn evaluate(&self, t: &LinearMovingTransform, derivative: bool) -> Result<(f64, Vec<f64>)> {
        let inputs = MiInputs {
            fixed: self.fixed.as_ref(),
            moving: self.moving.as_ref(),
            samples: &self.samples.points,
            fixed_transform: &self.chain,
            moving_transform: t,
            fixed_bin: Some(&self.samples.fixed_bin),
        };
        let mi = MattesMi {
            bins: self.samples.binning.bins,
        };
        let e = mi.evaluate(&self.samples.binning, &inputs, derivative)?;
        Ok((e.value, e.derivative))
    }
}

impl LinearEngine for CpuLinearEngine {
    type Level<'a> = CpuLevel<'a>;

    fn name(&self) -> String {
        "cpu-f64".to_string()
    }

    fn prepare_level<'a>(
        &'a self,
        fixed: Volume<f64>,
        moving: Volume<f64>,
        samples: PreparedSamples,
    ) -> Result<CpuLevel<'a>> {
        Ok(CpuLevel {
            fixed: Cow::Owned(fixed),
            moving: Cow::Owned(moving),
            samples,
            chain: TransformChain::new(),
        })
    }

    fn prepare_level_borrowed<'a>(
        &'a self,
        fixed: &'a Volume<f64>,
        moving: &'a Volume<f64>,
        samples: PreparedSamples,
    ) -> Result<CpuLevel<'a>> {
        Ok(CpuLevel {
            fixed: Cow::Borrowed(fixed),
            moving: Cow::Borrowed(moving),
            samples,
            chain: TransformChain::new(),
        })
    }
}

/// What one level of a linear stage did.
#[derive(Clone, Debug)]
pub struct LinearLevelTrace {
    /// Level index.
    pub level: usize,
    /// Shrink factors applied to the fixed grid.
    pub shrink_factors: [u32; 3],
    /// Voxels of the level grid.
    pub grid_size: [usize; 3],
    /// Samples that entered the metric.
    pub samples: usize,
    /// The optimiser trace.
    pub cg: CgTrace,
    /// Wall-clock seconds of the level (smoothing, sampling, metric set-up and optimisation).
    pub seconds: f64,
    /// Parameters at the end of the level.
    pub parameters: Vec<f64>,
}

/// Result of one linear stage.
#[derive(Clone, Debug)]
pub struct LinearStageResult {
    /// The optimised transform with its centre.
    pub transform: ParametricTransform,
    /// The same as an [`AffineTransform`].
    pub affine: AffineTransform,
    /// Per-level traces.
    pub levels: Vec<LinearLevelTrace>,
}

impl LinearStageResult {
    /// The metric value on the last `DIAGNOSTIC` line of the last level (what ANTs prints on
    /// stdout with `--verbose 1`). `None` when the last level took no iteration.
    pub fn final_metric(&self) -> Option<f64> {
        self.levels.last().and_then(|l| l.cg.final_value())
    }
}

/// Runs one linear stage: the optimised transform starts at `start` and is applied first, then
/// `outer` (the earlier transforms of the composite).
///
/// `fixed` and `moving` are the preprocessed full-resolution images of the first metric. Per
/// level: the fixed grid shrunk by the level factors is the virtual domain, both images are
/// smoothed at full resolution, one sample set is drawn with the next seed of the stage, the
/// metric is initialised, and ITK's conjugate-gradient optimiser runs.
pub fn run_linear_stage<E: LinearEngine>(
    engine: &E,
    recipe: &LinearRecipe,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    outer: AffineTransform,
    start: ParametricTransform,
) -> Result<LinearStageResult> {
    run_linear_stage_cached(
        engine,
        recipe,
        fixed,
        moving,
        outer,
        start,
        &mut SmoothCache::default(),
        None,
    )
}

/// Which image pair a [`SmoothCache`] holds smoothed copies of: the buffers, the grid and a
/// fingerprint of sampled voxel values, so a cache handed to another pair (or to a new image
/// that reuses a freed buffer) is emptied instead of answering with the wrong images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PairId {
    fixed_ptr: usize,
    moving_ptr: usize,
    fixed_len: usize,
    moving_len: usize,
    fixed_size: [usize; 3],
    probe: u64,
}

impl PairId {
    fn of(fixed: &Volume<f64>, moving: &Volume<f64>) -> Self {
        const PROBES: usize = 64;
        let mut probe = 0xcbf2_9ce4_8422_2325u64;
        for vol in [fixed, moving] {
            let data = vol.data();
            for i in 0..PROBES.min(data.len()) {
                let v = data[i * data.len() / PROBES.min(data.len())];
                probe = (probe ^ v.to_bits()).wrapping_mul(0x0100_0000_01b3);
            }
        }
        Self {
            fixed_ptr: fixed.data().as_ptr() as usize,
            moving_ptr: moving.data().as_ptr() as usize,
            fixed_len: fixed.data().len(),
            moving_len: moving.data().len(),
            fixed_size: fixed.grid().size(),
            probe,
        }
    }
}

type SmoothedPair<'r> = (Cow<'r, Volume<f64>>, Cow<'r, Volume<f64>>);

/// Smoothed copies of one image pair per sigma, shared by the stages of a chain (both linear
/// stages smooth the same full-resolution images with the same sigmas) and then by the SyN stage
/// ([`SmoothCache::take`]).
#[derive(Default)]
pub struct SmoothCache {
    owner: Option<PairId>,
    entries: Vec<(u64, bool, Volume<f64>, Volume<f64>)>,
}

impl SmoothCache {
    /// Drops every stored pair (the raw images stay with the caller).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.owner = None;
    }

    /// Removes and returns the smoothed pair stored for `sigma` and `units`, when the cache holds
    /// copies of exactly this `fixed` / `moving` pair.
    pub fn take(
        &mut self,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        sigma: f64,
        units: SigmaUnits,
    ) -> Option<(Volume<f64>, Volume<f64>)> {
        if self.owner != Some(PairId::of(fixed, moving)) {
            return None;
        }
        let voxels = units == SigmaUnits::Voxels;
        let key = sigma.to_bits();
        let at = self
            .entries
            .iter()
            .position(|(k, v, _, _)| *k == key && *v == voxels)?;
        let (_, _, f, m) = self.entries.swap_remove(at);
        Some((f, m))
    }

    fn get<'r>(
        &'r mut self,
        fixed: &'r Volume<f64>,
        moving: &'r Volume<f64>,
        sigma: f64,
        units: SigmaUnits,
        retain: bool,
    ) -> Result<SmoothedPair<'r>> {
        if sigma <= 0.0 {
            return Ok((Cow::Borrowed(fixed), Cow::Borrowed(moving)));
        }
        let id = PairId::of(fixed, moving);
        if self.owner != Some(id) {
            self.entries.clear();
            self.owner = Some(id);
        }
        let voxels = units == SigmaUnits::Voxels;
        let key = sigma.to_bits();
        let found = self
            .entries
            .iter()
            .position(|(k, v, _, _)| *k == key && *v == voxels);
        let at = match found {
            Some(at) => at,
            None if retain => {
                let (f, m) = rayon::join(
                    || smooth_for_level(fixed, sigma, units),
                    || smooth_for_level(moving, sigma, units),
                );
                self.entries.push((key, voxels, f?, m?));
                self.entries.len() - 1
            }
            None => {
                let f = smooth_for_level(fixed, sigma, units)?;
                let m = smooth_for_level(moving, sigma, units)?;
                return Ok((Cow::Owned(f), Cow::Owned(m)));
            }
        };
        let (_, _, f, m) = &self.entries[at];
        Ok((Cow::Borrowed(f), Cow::Borrowed(m)))
    }
}

/// [`run_linear_stage`] with an explicit cache of smoothed images.
#[allow(clippy::too_many_arguments)]
pub fn run_linear_stage_cached<E: LinearEngine>(
    engine: &E,
    recipe: &LinearRecipe,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    outer: AffineTransform,
    start: ParametricTransform,
    cache: &mut SmoothCache,
    cancel: Option<&AtomicBool>,
) -> Result<LinearStageResult> {
    recipe.validate()?;
    let expected = match recipe.kind {
        LinearKind::Rigid => 6,
        LinearKind::Affine => 12,
    };
    if start.number_of_parameters() != expected {
        return Err(invalid(
            "linear stage",
            "start transform does not match the stage kind",
        ));
    }
    let fixed_grid: Grid = *fixed.grid();
    let mut seeds = SeedSequence::new(recipe.seed);
    let spec = SamplingSpec {
        strategy: SamplingStrategy::Regular,
        percentage: recipe.sampling_percentage,
    };
    let mut current = LinearMovingTransform {
        outer,
        optimised: start,
    };
    let mut levels = Vec::with_capacity(recipe.levels());
    for level in 0..recipe.levels() {
        check_cancel(cancel)?;
        let t0 = Instant::now();
        let factors =
            shrink_factors_per_dimension(recipe.shrink_factors[level], fixed_grid.spacing());
        let grid = level_virtual_grid(&fixed_grid, factors)?;
        let sigma = recipe.smoothing_sigmas[level];
        let (fixed_s, moving_s) = cache.get(
            fixed,
            moving,
            sigma,
            recipe.sigma_units,
            recipe.cache_smoothed_images,
        )?;
        let mask = recipe.mask.as_ref();
        let keep = |p: Vec3| match mask {
            Some(m) => m.is_inside(p),
            None => true,
        };
        let raw = metric_sample_points(&grid, spec, &mut seeds, Some(&keep));
        let prepared = prepare_samples(&fixed_s, &moving_s, &raw, recipe.bins)?;
        let sample_count = prepared.points.len();
        let metric = engine.prepare_level_borrowed(&fixed_s, &moving_s, prepared)?;
        let corners = corner_points(&grid);
        let settings = CgSettings::ants(
            recipe.gradient_step,
            recipe.iterations[level],
            recipe.convergence_threshold,
            recipe.convergence_window,
        );
        let (next, cg) = optimise_level(&metric, &corners, current, &settings, cancel)?;
        current = next;
        levels.push(LinearLevelTrace {
            level,
            shrink_factors: factors,
            grid_size: grid.size(),
            samples: sample_count,
            cg,
            seconds: t0.elapsed().as_secs_f64(),
            parameters: current.optimised.parameters(),
        });
    }
    Ok(LinearStageResult {
        transform: current.optimised,
        affine: current.optimised.to_affine(),
        levels,
    })
}

/// The affine transform with the same map and centre `center`.
pub fn with_center(a: &AffineTransform, center: Vec3) -> AffineTransform {
    let offset = a.offset();
    let mc = mat3_mul_vec(&a.matrix, center);
    let translation = vec3_add(vec3_sub(offset, center), mc);
    AffineTransform::new(a.matrix, translation, center)
}

/// Result of [`run_linear_chain`].
#[derive(Clone, Debug)]
pub struct LinearChainResult {
    /// The centre-of-mass initial transform.
    pub initial: AffineTransform,
    /// The rigid stage.
    pub rigid: LinearStageResult,
    /// The affine stage.
    pub affine: LinearStageResult,
    /// `initial(rigid(affine(x)))` collapsed into one affine transform whose centre is the last
    /// linear transform's centre (what `0GenericAffine.mat` and composite element 0 hold).
    pub total: AffineTransform,
}

impl LinearChainResult {
    /// The metric value on the last `DIAGNOSTIC` line of the last (`Affine`) stage.
    pub fn final_metric(&self) -> Option<f64> {
        self.affine.final_metric()
    }
}

/// Centre-of-mass initialisation, then a `Rigid` and an `Affine` stage, each appended to the
/// composite (the optimised transform of a stage is applied first) and centred like the previous
/// linear transform; the result is collapsed as ANTs does.
///
/// `fixed_raw` / `moving_raw` feed the initialiser (the images as the command line names them);
/// `fixed` / `moving` are the preprocessed images the metrics see.
pub fn run_linear_chain<E: LinearEngine>(
    engine: &E,
    rigid: &LinearRecipe,
    affine: &LinearRecipe,
    fixed_raw: &Volume<f64>,
    moving_raw: &Volume<f64>,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
) -> Result<LinearChainResult> {
    run_linear_chain_cached(
        engine,
        rigid,
        affine,
        fixed_raw,
        moving_raw,
        fixed,
        moving,
        &mut SmoothCache::default(),
        None,
    )
}

/// [`run_linear_chain`] with an explicit cache of smoothed images, left filled for the SyN stage
/// ([`crate::syn::run_syn_cached`]).
#[allow(clippy::too_many_arguments)]
pub fn run_linear_chain_cached<E: LinearEngine>(
    engine: &E,
    rigid: &LinearRecipe,
    affine: &LinearRecipe,
    fixed_raw: &Volume<f64>,
    moving_raw: &Volume<f64>,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    cache: &mut SmoothCache,
    cancel: Option<&AtomicBool>,
) -> Result<LinearChainResult> {
    if rigid.kind != LinearKind::Rigid || affine.kind != LinearKind::Affine {
        return Err(invalid(
            "linear chain",
            "expected a rigid then an affine recipe",
        ));
    }
    let initial = initial_transform(fixed_raw, moving_raw, InitFeature::CentreOfMass)?;
    let rigid_result = run_linear_stage_cached(
        engine,
        rigid,
        fixed,
        moving,
        initial,
        ParametricTransform::euler3d([0.0; 6], initial.center),
        cache,
        cancel,
    )?;
    let after_rigid = AffineTransform::compose(&initial, &rigid_result.affine);
    let start_affine = ParametricTransform::affine(&AffineTransform::new(
        crate::linalg::IDENTITY3,
        [0.0; 3],
        rigid_result.affine.center,
    ));
    let affine_result = run_linear_stage_cached(
        engine,
        affine,
        fixed,
        moving,
        after_rigid,
        start_affine,
        cache,
        cancel,
    )?;
    let collapsed = AffineTransform::compose(&after_rigid, &affine_result.affine);
    let total = with_center(&collapsed, affine_result.affine.center);
    Ok(LinearChainResult {
        initial,
        rigid: rigid_result,
        affine: affine_result,
        total,
    })
}

/// Result of [`run_rigid_registration`].
#[derive(Clone, Debug)]
pub struct RigidRegistrationResult {
    /// The centre-of-mass initial transform.
    pub initial: AffineTransform,
    /// The rigid stage.
    pub rigid: LinearStageResult,
    /// `initial(rigid(x))` collapsed into one affine transform (what `0GenericAffine.mat` holds).
    pub total: AffineTransform,
}

impl RigidRegistrationResult {
    /// The metric value on the last `DIAGNOSTIC` line of the last level: the value ANTs prints
    /// on stdout and the score the multi-start picks its best start with.
    pub fn final_metric(&self) -> Option<f64> {
        self.rigid.final_metric()
    }
}

/// Centre-of-mass initialisation followed by one `Rigid` stage, the shape of the
/// `--initial-moving-transform [fixed,moving,1] --transform Rigid[...]` call that registers a
/// second pre-op contrast onto the anchor image.
///
/// `fixed` and `moving` are the images exactly as the command line names them (no winsorising);
/// both the initialiser and the metric see them.
pub fn run_rigid_registration<E: LinearEngine>(
    engine: &E,
    recipe: &LinearRecipe,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
) -> Result<RigidRegistrationResult> {
    run_rigid_registration_cancellable(engine, recipe, fixed, moving, None)
}

pub fn run_rigid_registration_cancellable<E: LinearEngine>(
    engine: &E,
    recipe: &LinearRecipe,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    cancel: Option<&AtomicBool>,
) -> Result<RigidRegistrationResult> {
    if recipe.kind != LinearKind::Rigid {
        return Err(invalid("rigid registration", "expected a rigid recipe"));
    }
    check_cancel(cancel)?;
    let initial = initial_transform(fixed, moving, InitFeature::CentreOfMass)?;
    run_rigid_registration_from_cancellable(engine, recipe, fixed, moving, initial, cancel)
}

/// One `Rigid` stage from an arbitrary initial transform, the shape of
/// `--initial-moving-transform <start> --transform Rigid[...]`: ANTs puts `<start>` into the
/// moving initial composite and optimises a fresh `Euler3DTransform` on top, then collapses
/// `[start, rigid]` into one affine whose centre is the rigid transform's.
pub fn run_rigid_registration_from<E: LinearEngine>(
    engine: &E,
    recipe: &LinearRecipe,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    initial: AffineTransform,
) -> Result<RigidRegistrationResult> {
    run_rigid_registration_from_cancellable(engine, recipe, fixed, moving, initial, None)
}

pub fn run_rigid_registration_from_cancellable<E: LinearEngine>(
    engine: &E,
    recipe: &LinearRecipe,
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    initial: AffineTransform,
    cancel: Option<&AtomicBool>,
) -> Result<RigidRegistrationResult> {
    if recipe.kind != LinearKind::Rigid {
        return Err(invalid("rigid registration", "expected a rigid recipe"));
    }
    let rigid = run_linear_stage_cached(
        engine,
        recipe,
        fixed,
        moving,
        initial,
        ParametricTransform::euler3d([0.0; 6], initial.center),
        &mut SmoothCache::default(),
        cancel,
    )?;
    let collapsed = AffineTransform::compose(&initial, &rigid.affine);
    let total = with_center(&collapsed, rigid.affine.center);
    Ok(RigidRegistrationResult {
        initial,
        rigid,
        total,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_range_loop)]
mod tests {
    use super::*;
    use crate::linalg::{mat3_max_abs_diff, IDENTITY3};
    use crate::transform::ParametricTransform;

    fn volume(n: usize, spacing: f64, pose: &AffineTransform) -> Volume<f64> {
        let g = Grid::new([n, n, n], [spacing; 3], [0.0; 3], IDENTITY3).unwrap();
        let c = (n as f64 - 1.0) * spacing / 2.0;
        let centres = [
            ([0.0, 0.0, 0.0], 9.0, 0.7),
            ([10.0, 4.0, -3.0], 4.5, 0.9),
            ([-9.0, 7.0, 5.0], 5.0, 0.8),
            ([3.0, -10.0, 6.0], 4.0, 0.6),
            ([-4.0, -5.0, -10.0], 4.5, 0.85),
        ];
        let inv = pose.inverse().unwrap();
        let data = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let p = g.index_to_point([i as f64, j as f64, k as f64]);
                let q = inv.apply(p);
                centres
                    .iter()
                    .map(|(b, s, a)| {
                        let d2 = (q[0] - c - b[0]).powi(2)
                            + (q[1] - c - b[1]).powi(2)
                            + (q[2] - c - b[2]).powi(2);
                        a * (-d2 / (2.0 * s * s)).exp()
                    })
                    .sum::<f64>()
                    / 1.6
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    }

    fn small_recipe(kind: LinearKind) -> LinearRecipe {
        LinearRecipe {
            iterations: vec![60, 40, 30],
            shrink_factors: vec![4, 2, 1],
            smoothing_sigmas: vec![2.0, 1.0, 0.0],
            ..LinearRecipe::cohort(kind)
        }
    }

    fn chain_recipe(kind: LinearKind) -> LinearRecipe {
        LinearRecipe {
            iterations: vec![15, 10, 8],
            ..small_recipe(kind)
        }
    }

    fn one_level_recipe() -> LinearRecipe {
        LinearRecipe {
            iterations: vec![1],
            shrink_factors: vec![2],
            smoothing_sigmas: vec![1.0],
            ..LinearRecipe::cohort(LinearKind::Rigid)
        }
    }

    #[test]
    fn cohort_recipes_match_the_pipeline_argv() {
        let r = LinearRecipe::cohort_rigid();
        assert_eq!(r.iterations, vec![1000, 500, 250, 100]);
        assert_eq!(r.shrink_factors, vec![8, 4, 2, 1]);
        assert_eq!(r.smoothing_sigmas, vec![3.0, 2.0, 1.0, 0.0]);
        assert_eq!(
            (r.bins, r.sampling_percentage, r.seed),
            (32, 0.25, 20_260_910)
        );
        assert!(r.validate().is_ok());
        let mut bad = r.clone();
        bad.shrink_factors.pop();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn collapse_keeps_the_map_and_changes_only_the_centre() {
        let a = AffineTransform::new(
            [[0.9, 0.1, 0.0], [-0.1, 1.1, 0.05], [0.0, 0.0, 1.0]],
            [1.0, -2.0, 3.0],
            [5.0, 6.0, 7.0],
        );
        let b = with_center(&a, [-3.0, 2.0, 9.0]);
        for p in [[0.0, 0.0, 0.0], [10.0, -4.0, 8.0]] {
            let (x, y) = (a.apply(p), b.apply(p));
            for d in 0..3 {
                assert!((x[d] - y[d]).abs() < 1e-12);
            }
        }
        assert_eq!(mat3_max_abs_diff(&a.matrix, &b.matrix), 0.0);
    }

    #[test]
    fn the_rigid_stage_recovers_a_known_pose_on_the_cpu_reference() {
        let c = 21.0;
        let truth = AffineTransform::new(
            ParametricTransform::euler3d([0.04, -0.03, 0.06, 2.0, -1.5, 1.0], [c; 3])
                .to_affine()
                .matrix,
            [2.0, -1.5, 1.0],
            [c; 3],
        );
        let fixed = volume(43, 1.0, &AffineTransform::identity());
        let moving = volume(43, 1.0, &truth);
        let out = run_linear_stage(
            &CpuLinearEngine,
            &small_recipe(LinearKind::Rigid),
            &fixed,
            &moving,
            AffineTransform::identity(),
            ParametricTransform::euler3d([0.0; 6], [c; 3]),
        )
        .unwrap();
        let pts = [[10.0, 10.0, 10.0], [30.0, 12.0, 25.0], [20.0, 32.0, 14.0]];
        for p in pts {
            let (got, want) = (out.affine.apply(p), truth.apply(p));
            let e = ((got[0] - want[0]).powi(2)
                + (got[1] - want[1]).powi(2)
                + (got[2] - want[2]).powi(2))
            .sqrt();
            assert!(e < 0.6, "point {p:?}: error {e}");
        }
        assert_eq!(out.levels.len(), 3);
        assert!(out.levels.iter().all(|l| l.samples > 100));
    }

    #[test]
    fn the_chain_aligns_a_shifted_pair_and_writes_a_collapsed_affine() {
        let shift = AffineTransform::from_translation([3.0, -2.0, 1.5]);
        let fixed = volume(43, 1.0, &AffineTransform::identity());
        let moving = volume(43, 1.0, &shift);
        let out = run_linear_chain(
            &CpuLinearEngine,
            &chain_recipe(LinearKind::Rigid),
            &chain_recipe(LinearKind::Affine),
            &fixed,
            &moving,
            &fixed,
            &moving,
        )
        .unwrap();
        assert_eq!(out.total.center, out.affine.affine.center);
        for p in [[10.0, 10.0, 10.0], [30.0, 12.0, 25.0]] {
            let (got, want) = (out.total.apply(p), shift.apply(p));
            let e = ((got[0] - want[0]).powi(2)
                + (got[1] - want[1]).powi(2)
                + (got[2] - want[2]).powi(2))
            .sqrt();
            assert!(e < 0.8, "point {p:?}: error {e}");
        }
    }

    #[test]
    fn the_rigid_registration_aligns_a_shifted_pair_and_rejects_an_affine_recipe() {
        let shift = AffineTransform::from_translation([3.0, -2.0, 1.5]);
        let fixed = volume(43, 1.0, &AffineTransform::identity());
        let moving = volume(43, 1.0, &shift);
        let out = run_rigid_registration(
            &CpuLinearEngine,
            &small_recipe(LinearKind::Rigid),
            &fixed,
            &moving,
        )
        .unwrap();
        assert_eq!(out.total.center, out.rigid.affine.center);
        for p in [[10.0, 10.0, 10.0], [30.0, 12.0, 25.0]] {
            let (got, want) = (out.total.apply(p), shift.apply(p));
            let e = ((got[0] - want[0]).powi(2)
                + (got[1] - want[1]).powi(2)
                + (got[2] - want[2]).powi(2))
            .sqrt();
            assert!(e < 0.8, "point {p:?}: error {e}");
        }
        assert!(run_rigid_registration(
            &CpuLinearEngine,
            &small_recipe(LinearKind::Affine),
            &fixed,
            &moving
        )
        .is_err());
    }

    fn dist(a: Vec3, b: Vec3) -> f64 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    #[test]
    fn a_supplied_start_is_used_and_the_final_metric_is_reported() {
        let c = 21.0;
        let start = AffineTransform::from_translation([2.0, -1.0, 0.5]);
        let truth = AffineTransform::from_translation([3.0, -2.0, 1.5]);
        let fixed = volume(43, 1.0, &AffineTransform::identity());
        let moving = volume(43, 1.0, &truth);
        let out = run_rigid_registration_from(
            &CpuLinearEngine,
            &small_recipe(LinearKind::Rigid),
            &fixed,
            &moving,
            start,
        )
        .unwrap();
        assert_eq!(out.initial, start);
        assert_eq!(out.total.center, out.rigid.affine.center);
        let metric = out.final_metric().expect("the last level took iterations");
        assert!(metric.is_finite());
        assert_eq!(
            out.rigid.levels.last().unwrap().cg.final_value(),
            Some(metric)
        );
        let probe = [c, c, c];
        let start_err = dist(start.apply(probe), truth.apply(probe));
        let total_err = dist(out.total.apply(probe), truth.apply(probe));
        assert!(
            total_err <= start_err + 0.2,
            "start {start_err} total {total_err}"
        );
    }

    #[test]
    fn a_half_mask_shrinks_the_sample_set_of_a_stage() {
        let fixed = volume(43, 1.0, &AffineTransform::identity());
        let moving = volume(43, 1.0, &AffineTransform::identity());
        let start = ParametricTransform::euler3d([0.0; 6], [21.0; 3]);
        let base = run_linear_stage(
            &CpuLinearEngine,
            &one_level_recipe(),
            &fixed,
            &moving,
            AffineTransform::identity(),
            start,
        )
        .unwrap();
        let g = *fixed.grid();
        let half = g.size()[0] / 2;
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|lin| f64::from(u8::from(g.ijk(lin)[0] >= half)))
            .collect();
        let mask = MaskVolume::from_volume(Volume::from_vec(g, data).unwrap());
        let recipe = one_level_recipe().with_mask(mask);
        let masked = run_linear_stage(
            &CpuLinearEngine,
            &recipe,
            &fixed,
            &moving,
            AffineTransform::identity(),
            start,
        )
        .unwrap();
        assert!(masked.levels[0].samples < base.levels[0].samples);
    }

    #[test]
    fn the_smooth_cache_only_answers_for_the_pair_it_was_filled_with() {
        let a = volume(12, 2.0, &AffineTransform::identity());
        let b = volume(12, 2.0, &AffineTransform::identity());
        let other = volume(
            12,
            2.0,
            &AffineTransform::new(IDENTITY3, [3.0, 0.0, 0.0], [0.0; 3]),
        );
        let mut cache = SmoothCache::default();
        let (f, m) = cache.get(&a, &b, 1.5, SigmaUnits::Voxels, true).unwrap();
        let (f, m) = (f.into_owned(), m.into_owned());
        assert_eq!(f, smooth_for_level(&a, 1.5, SigmaUnits::Voxels).unwrap());
        assert_eq!(m, smooth_for_level(&b, 1.5, SigmaUnits::Voxels).unwrap());
        assert!(cache.take(&a, &other, 1.5, SigmaUnits::Voxels).is_none());
        assert!(cache.take(&a, &b, 1.5, SigmaUnits::Millimetres).is_none());
        assert!(cache.take(&a, &b, 2.5, SigmaUnits::Voxels).is_none());
        let (tf, tm) = cache.take(&a, &b, 1.5, SigmaUnits::Voxels).unwrap();
        assert_eq!((tf, tm), (f, m));
        assert!(cache.take(&a, &b, 1.5, SigmaUnits::Voxels).is_none());

        cache.get(&a, &b, 1.5, SigmaUnits::Voxels, true).unwrap();
        cache
            .get(&a, &other, 1.5, SigmaUnits::Voxels, true)
            .unwrap();
        assert!(cache.take(&a, &b, 1.5, SigmaUnits::Voxels).is_none());
    }
}
