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
// The stage chaining generalises `run_linear_chain_cached` in `linear.rs` (itself after ITK
// `itkImageRegistrationMethodv4.hxx`, `InitializeCenterOfLinearOutputTransform`, and ANTs
// `itkantsRegistrationHelper.hxx`, `AddLinearTransformToCompositeTransform` /
// `CollapseLinearTransforms`) from a fixed Rigid-then-Affine pair to any list of linear stages
// after any initial transform. See `docs/THIRD_PARTY_PORTS.md`.

//! Stage-list orchestration: an initial transform, any number of `Rigid` / `Affine` stages in any
//! order, then at most one SyN stage, as `antsRegistration` runs them with
//! `--collapse-output-transforms 1`, and the ANTs-named outputs of such a run.
//!
//! [`register_stages`] with [`StagePlan::cohort`] runs exactly what
//! [`crate::register::register_full`] runs (bit for bit; a test proves it).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::apply::{apply_chain, Interpolation};
use crate::backend::Backend;
use crate::error::{invalid, Result};
use crate::grid::Grid;
use crate::init::{initial_transform, InitFeature};
use crate::linalg::IDENTITY3;
use crate::linear::{
    run_linear_stage_cached, with_center, LinearEngine, LinearKind, LinearRecipe,
    LinearStageResult, SmoothCache,
};
use crate::recipe::SynRecipe;
use crate::register::{
    check_winsorize, prepare_channels_with, write_linear_output, LinearPlan, PreparedChannels,
    RegisterTimings, WINSORIZE_LOWER, WINSORIZE_UPPER,
};
use crate::syn::{run_syn_pair_cached, SynLevelTrace, SynOptions};
use crate::transform::ants_io::{
    with_suffix, AFFINE_SUFFIX, COMPOSITE_SUFFIX, INVERSE_COMPOSITE_SUFFIX, INVERSE_WARP_SUFFIX,
    WARP_SUFFIX,
};
use crate::transform::field_nifti::write_field_nifti;
use crate::transform::{
    write_composite_pair, AffineTransform, CompositePair, DisplacementField, ParametricTransform,
    TransformChain, TransformElem,
};
use crate::volume::Volume;

/// Where the chain starts (`--initial-moving-transform`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InitialTransform {
    /// `[fixed,moving,1]`: the translation between the centres of intensity mass of the raw
    /// (resampled, not preprocessed) first channel.
    CentreOfMass,
    /// `Identity`: the identity, centred at the origin, so a first linear stage rotates about
    /// the origin as in ANTs.
    Identity,
    /// A given affine (fixed points to moving points), e.g. read from a `.mat` file; the first
    /// linear stage is centred at its centre.
    Affine(AffineTransform),
}

/// What runs: the start, the linear stages in order, an optional SyN stage after them, and the
/// intensity preprocessing of every metric image.
#[derive(Clone, Debug, PartialEq)]
pub struct StagePlan {
    /// Start of the chain.
    pub initial: InitialTransform,
    /// `Rigid` / `Affine` stages, run in this order on the first channel (any number, also none).
    pub linear: Vec<LinearRecipe>,
    /// The SyN stage on every channel, after the linear stages; `None` for a linear-only run.
    pub syn: Option<SynRecipe>,
    /// `Some((lower, upper))`: `--winsorize-image-intensities [lower,upper]`; `None`: ANTs'
    /// default rescale to `[0, 1]` with the quantile window `(0, 1)`.
    pub winsorize: Option<(f32, f32)>,
}

impl StagePlan {
    /// The cohort chain with `plan`'s linear stages and `syn`: centre of mass, `Rigid`, `Affine`,
    /// SyN, winsorised to `[0.005, 0.995]`.
    pub fn cohort(plan: &LinearPlan, syn: &SynRecipe) -> Self {
        Self {
            initial: InitialTransform::CentreOfMass,
            linear: vec![plan.rigid.clone(), plan.affine.clone()],
            syn: Some(syn.clone()),
            winsorize: Some((WINSORIZE_LOWER, WINSORIZE_UPPER)),
        }
    }

    /// Checks every stage and the preprocessing window; a plan needs at least one stage.
    pub fn validate(&self) -> Result<()> {
        if self.linear.is_empty() && self.syn.is_none() {
            return Err(invalid("stage plan", "the plan has no stage"));
        }
        for stage in &self.linear {
            stage.validate()?;
        }
        if let Some(syn) = &self.syn {
            syn.validate()?;
        }
        if let InitialTransform::Affine(a) = &self.initial {
            let finite = a
                .parameters()
                .iter()
                .chain(&a.center)
                .all(|v| v.is_finite());
            if !finite {
                return Err(invalid(
                    "stage plan",
                    "the initial transform has non-finite values",
                ));
            }
            a.inverse()
                .map_err(|_| invalid("stage plan", "the initial transform is singular"))?;
        }
        check_winsorize(self.winsorize)
    }

    /// Whether the plan ends with a SyN stage.
    pub fn is_deformable(&self) -> bool {
        self.syn.is_some()
    }
}

/// Runs `stages` one after another from `initial`: each stage optimises a fresh transform of its
/// kind, started as the identity centred where the previous linear transform is centred and
/// applied before everything earlier; the result is collapsed into one affine centred like the
/// last stage. With no stage, the result is `initial` itself.
///
/// `fixed` / `moving` are the preprocessed images of the first channel.
#[allow(clippy::too_many_arguments)]
pub fn run_linear_stages<E: LinearEngine>(
    engine: &E,
    initial: AffineTransform,
    stages: &[LinearRecipe],
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    cache: &mut SmoothCache,
    cancel: Option<&AtomicBool>,
) -> Result<(Vec<LinearStageResult>, AffineTransform)> {
    let mut current = initial;
    let mut center = initial.center;
    let mut results = Vec::with_capacity(stages.len());
    for recipe in stages {
        let start = match recipe.kind {
            LinearKind::Rigid => ParametricTransform::euler3d([0.0; 6], center),
            LinearKind::Affine => {
                ParametricTransform::affine(&AffineTransform::new(IDENTITY3, [0.0; 3], center))
            }
        };
        let stage =
            run_linear_stage_cached(engine, recipe, fixed, moving, current, start, cache, cancel)?;
        current = AffineTransform::compose(&current, &stage.affine);
        center = stage.affine.center;
        results.push(stage);
    }
    let total = if results.is_empty() {
        initial
    } else {
        with_center(&current, center)
    };
    Ok((results, total))
}

/// Result of [`register_stages`].
#[derive(Clone, Debug)]
pub struct StagesOutput {
    /// With SyN: `Composite = [Affine, Warp]`, `InverseComposite = [InverseWarp, Affine^-1]`;
    /// linear-only: `[Affine]` and `[Affine^-1]` (what ANTs 2.6.5 writes for such a run with
    /// `--write-composite-transform 1`).
    pub pair: CompositePair,
    /// The first moving channel through the forward chain onto the registration grid.
    pub warped: Volume<f64>,
    /// The registration grid (grid of the fields).
    pub grid: Grid,
    /// The start of the chain.
    pub initial: AffineTransform,
    /// One result per linear stage, in order.
    pub linear: Vec<LinearStageResult>,
    /// The start and every linear stage collapsed into one affine (`0GenericAffine.mat`).
    pub total: AffineTransform,
    /// Per-level traces of the SyN stage; empty without one.
    pub levels: Vec<SynLevelTrace>,
    /// Whether a SyN stage ran (the pair holds displacement fields).
    pub deformable: bool,
    /// Phase timings.
    pub timings: RegisterTimings,
}

/// Runs `plan`: resampling to `iso_mm` and preprocessing every channel, the start, the linear
/// stages on `engine` (first channel), then the SyN stage on `backend` (every channel) from the
/// collapsed affine. Between the linear stages and the SyN stage the backend frees its idle
/// buffers ([`Backend::release_idle_buffers`]), so the device peak is that of one of the two
/// phases. A plan without a SyN stage takes one channel. The GPU capacity of the grid is the
/// caller's to check.
///
/// The linear stages share one cache of smoothed full-resolution images (one pair per distinct
/// sigma, when the stages keep `cache_smoothed_images`) on the host until the SyN stage has
/// taken what it reuses; a long stage list with many different sigmas holds that many pairs.
#[allow(clippy::too_many_arguments)]
pub fn register_stages<B: Backend, E: LinearEngine>(
    backend: &B,
    engine: &E,
    plan: &StagePlan,
    fixed: &[Volume<f64>],
    moving: &[Volume<f64>],
    iso_mm: Option<f64>,
    options: &SynOptions<'_>,
) -> Result<StagesOutput> {
    plan.validate()?;
    match &plan.syn {
        Some(syn) if syn.channel_weights.len() != fixed.len() => {
            return Err(invalid(
                "stage plan",
                "the number of channels differs from the number of channel weights",
            ));
        }
        None if fixed.len() > 1 => {
            return Err(invalid(
                "stage plan",
                "a plan without a SyN stage registers one channel; the linear stages use the first",
            ));
        }
        _ => {}
    }
    let mut timings = RegisterTimings::default();
    let PreparedChannels {
        channels,
        fixed_raw,
        moving_raw,
    } = prepare_channels_with(fixed, moving, iso_mm, plan.winsorize, &mut timings)?;
    let t = Instant::now();
    let initial = match plan.initial {
        InitialTransform::CentreOfMass => {
            initial_transform(&fixed_raw[0], &moving_raw[0], InitFeature::CentreOfMass)?
        }
        InitialTransform::Identity => AffineTransform::identity(),
        InitialTransform::Affine(a) => a,
    };
    let mut cache = SmoothCache::default();
    let (linear, total) = run_linear_stages(
        engine,
        initial,
        &plan.linear,
        &channels[0].fixed,
        &channels[0].moving,
        &mut cache,
        options.cancel,
    )?;
    timings.linear_s = t.elapsed().as_secs_f64();
    drop(fixed_raw);
    let moving_first = moving_raw
        .into_iter()
        .next()
        .ok_or_else(|| invalid("register", "no moving channel"))?;
    let (pair, levels) = match &plan.syn {
        Some(recipe) => {
            if !plan.linear.is_empty() {
                backend.release_idle_buffers();
            }
            let t = Instant::now();
            let syn = run_syn_pair_cached(
                backend,
                recipe,
                &channels,
                &total,
                options,
                Some(&mut cache),
            );
            drop(cache);
            let out = syn?;
            timings.syn_s = t.elapsed().as_secs_f64();
            out
        }
        None => {
            drop(cache);
            (CompositePair::linear(&total)?, Vec::new())
        }
    };
    let grid = *channels[0].fixed.grid();
    drop(channels);
    let t = Instant::now();
    let warped = apply_chain(
        &moving_first,
        &grid,
        &pair.forward,
        Interpolation::Linear,
        0.0,
    )?;
    timings.warp_s = t.elapsed().as_secs_f64();
    Ok(StagesOutput {
        pair,
        warped,
        grid,
        initial,
        linear,
        total,
        levels,
        deformable: plan.syn.is_some(),
        timings,
    })
}

/// Which transform files [`write_stage_outputs`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputLayout {
    /// `Composite.h5`, `InverseComposite.h5` and `0GenericAffine.mat` (what `register_syn` has
    /// always written; ANTs `--write-composite-transform 1` writes the two composites).
    Composite,
    /// ANTs' `--collapse-output-transforms 1 --write-composite-transform 0` files:
    /// `0GenericAffine.mat`, plus `1Warp.nii.gz` and `1InverseWarp.nii.gz` when a SyN stage ran.
    Split,
    /// Both sets.
    Both,
}

fn field_at(chain: &TransformChain, index: usize) -> Result<&DisplacementField> {
    match chain.queue().get(index) {
        Some(TransformElem::DisplacementField(f)) => Ok(f),
        _ => Err(invalid(
            "write outputs",
            "the composite pair holds no displacement field where one was expected",
        )),
    }
}

/// Writes the transforms of `out` under `prefix` in `layout` (ANTs naming, the prefix taken as
/// text) and, with `write_warped`, `<prefix>_Warped.nii.gz`; returns the paths written, in the
/// order composites, `0GenericAffine.mat`, warp, inverse warp, warped image.
pub fn write_stage_outputs(
    prefix: &Path,
    out: &StagesOutput,
    layout: OutputLayout,
    write_warped: bool,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    if matches!(layout, OutputLayout::Composite | OutputLayout::Both) {
        write_composite_pair(prefix, &out.pair)?;
        written.push(with_suffix(prefix, COMPOSITE_SUFFIX));
        written.push(with_suffix(prefix, INVERSE_COMPOSITE_SUFFIX));
    }
    write_linear_output(prefix, &out.total)?;
    written.push(with_suffix(prefix, AFFINE_SUFFIX));
    if out.deformable && matches!(layout, OutputLayout::Split | OutputLayout::Both) {
        let warp = with_suffix(prefix, WARP_SUFFIX);
        write_field_nifti(&warp, field_at(&out.pair.forward, 1)?)?;
        written.push(warp);
        let inverse = with_suffix(prefix, INVERSE_WARP_SUFFIX);
        write_field_nifti(&inverse, field_at(&out.pair.inverse, 0)?)?;
        written.push(inverse);
    }
    if write_warped {
        let path = with_suffix(prefix, "_Warped.nii.gz");
        let f32_data: Vec<f32> = out.warped.data().iter().map(|v| *v as f32).collect();
        Volume::from_vec(*out.warped.grid(), f32_data)?.save_nifti(&path)?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cpu::CpuBackend;
    use crate::linear::CpuLinearEngine;
    use crate::register::register_full;
    use crate::transform::{read_composite_pair, read_transform_file};

    fn blob_volume(n: usize, spacing: f64, shift: [f64; 3]) -> Volume<f64> {
        let g = Grid::new([n, n, n], [spacing; 3], [0.0; 3], IDENTITY3).unwrap();
        let c = (n as f64 - 1.0) * spacing / 2.0;
        let data = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let p = g.index_to_point([i as f64, j as f64, k as f64]);
                let r2 = (p[0] - c - shift[0]).powi(2)
                    + (p[1] - c - shift[1]).powi(2)
                    + (p[2] - c - shift[2]).powi(2);
                let lobe = (p[0] - c - shift[0] - 6.0).powi(2)
                    + (p[1] - c - shift[1] + 3.0).powi(2)
                    + (p[2] - c - shift[2]).powi(2);
                let noise = ((lin * 2654435761) % 1000) as f64 / 20000.0;
                (-r2 / (2.0 * (0.22 * n as f64 * spacing).powi(2))).exp()
                    + 0.6 * (-lobe / 18.0).exp()
                    + noise
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    }

    fn small(r: LinearRecipe) -> LinearRecipe {
        LinearRecipe {
            iterations: vec![12, 8],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..r
        }
    }

    fn small_syn() -> SynRecipe {
        SynRecipe {
            iterations: vec![6, 4],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..SynRecipe::cohort()
        }
    }

    fn pair() -> (Vec<Volume<f64>>, Vec<Volume<f64>>) {
        (
            vec![blob_volume(24, 2.0, [0.0; 3])],
            vec![blob_volume(24, 2.0, [4.0, -2.0, 1.0])],
        )
    }

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("fastwarp_reg_stages_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_cohort_plan_reproduces_register_full_bit_for_bit() {
        let (fx, mv) = pair();
        let linear = LinearPlan {
            rigid: small(LinearRecipe::cohort_rigid()),
            affine: small(LinearRecipe::cohort_affine()),
        };
        let syn = small_syn();
        let options = SynOptions::default();
        let full = register_full(
            &CpuBackend,
            &CpuLinearEngine,
            &linear,
            &syn,
            &fx,
            &mv,
            Some(2.0),
            &options,
        )
        .unwrap();
        let staged = register_stages(
            &CpuBackend,
            &CpuLinearEngine,
            &StagePlan::cohort(&linear, &syn),
            &fx,
            &mv,
            Some(2.0),
            &options,
        )
        .unwrap();
        let chain = full.linear.as_ref().unwrap();
        assert_eq!(staged.initial, chain.initial);
        assert_eq!(staged.total, chain.total);
        assert_eq!(staged.linear.len(), 2);
        assert_eq!(staged.linear[0].affine, chain.rigid.affine);
        assert_eq!(staged.linear[1].affine, chain.affine.affine);
        assert_eq!(staged.pair, full.pair);
        assert_eq!(staged.warped, full.warped);
        assert_eq!(staged.levels, full.levels);
        assert!(staged.deformable);
    }

    #[test]
    fn a_linear_only_plan_writes_the_collapsed_affine_and_linear_composites() {
        let (fx, mv) = pair();
        let plan = StagePlan {
            initial: InitialTransform::CentreOfMass,
            linear: vec![
                small(LinearRecipe::cohort_rigid()),
                small(LinearRecipe::cohort_affine()),
                small(LinearRecipe::cohort_rigid()),
            ],
            syn: None,
            winsorize: None,
        };
        let out = register_stages(
            &CpuBackend,
            &CpuLinearEngine,
            &plan,
            &fx,
            &mv,
            None,
            &SynOptions::default(),
        )
        .unwrap();
        assert!(!out.deformable && out.levels.is_empty());
        assert_eq!(out.linear.len(), 3);
        assert_eq!(out.total.center, out.linear[2].affine.center);
        let c = 23.0;
        let mapped = out.total.apply([c, c, c]);
        let want = [c + 4.0, c - 2.0, c + 1.0];
        for a in 0..3 {
            assert!((mapped[a] - want[a]).abs() < 1.0, "{mapped:?}");
        }
        let d = tmp("linear");
        let prefix = d.join("o_");
        let files = write_stage_outputs(&prefix, &out, OutputLayout::Both, true).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "o_Composite.h5",
                "o_InverseComposite.h5",
                "o_0GenericAffine.mat",
                "o__Warped.nii.gz"
            ]
        );
        let back = read_composite_pair(&prefix).unwrap();
        assert_eq!(back.forward.len(), 1);
        assert_eq!(back.inverse.len(), 1);
        let p = [5.0, 9.0, 11.0];
        let q = back.inverse.apply(back.forward.apply(p));
        for a in 0..3 {
            assert!((q[a] - p[a]).abs() < 1e-9);
        }
        let mat = read_transform_file(&with_suffix(&prefix, AFFINE_SUFFIX)).unwrap();
        assert_eq!(mat.queue(), &[TransformElem::Affine(out.total)]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_syn_only_plan_from_the_identity_writes_the_identity_affine_and_split_fields() {
        let (fx, mv) = pair();
        let plan = StagePlan {
            initial: InitialTransform::Identity,
            linear: Vec::new(),
            syn: Some(small_syn()),
            winsorize: Some((WINSORIZE_LOWER, WINSORIZE_UPPER)),
        };
        let out = register_stages(
            &CpuBackend,
            &CpuLinearEngine,
            &plan,
            &fx,
            &mv,
            None,
            &SynOptions::default(),
        )
        .unwrap();
        assert!(out.deformable && out.linear.is_empty());
        assert_eq!(out.total, AffineTransform::identity());
        assert_eq!(out.levels.len(), 2);
        let d = tmp("synonly");
        let prefix = d.join("o_");
        let files = write_stage_outputs(&prefix, &out, OutputLayout::Split, false).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "o_0GenericAffine.mat",
                "o_1Warp.nii.gz",
                "o_1InverseWarp.nii.gz"
            ]
        );
        assert!(!d.join("o_Composite.h5").exists());
        let warp = read_transform_file(&d.join("o_1Warp.nii.gz")).unwrap();
        let mat = read_transform_file(&d.join("o_0GenericAffine.mat")).unwrap();
        let mut chain = mat;
        chain.extend(warp);
        let err = |a: &Volume<f64>| {
            a.data()
                .iter()
                .zip(fx[0].data())
                .map(|(x, y)| (x - y).powi(2))
                .sum::<f64>()
        };
        let resampled =
            apply_chain(&mv[0], fx[0].grid(), &chain, Interpolation::Linear, 0.0).unwrap();
        assert!(err(&resampled) < err(&mv[0]));
        let probe = [20.0, 21.0, 25.0];
        let a = chain.apply(probe);
        let b = out.pair.forward.apply(probe);
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() < 1e-5, "{a:?} {b:?}");
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_given_affine_starts_the_chain_and_centres_the_first_stage() {
        let (fx, mv) = pair();
        let start = AffineTransform::new(IDENTITY3, [3.0, -1.5, 0.5], [10.0, 12.0, 14.0]);
        let plan = StagePlan {
            initial: InitialTransform::Affine(start),
            linear: vec![small(LinearRecipe::cohort_affine())],
            syn: None,
            winsorize: Some((WINSORIZE_LOWER, WINSORIZE_UPPER)),
        };
        let out = register_stages(
            &CpuBackend,
            &CpuLinearEngine,
            &plan,
            &fx,
            &mv,
            None,
            &SynOptions::default(),
        )
        .unwrap();
        assert_eq!(out.initial, start);
        assert_eq!(out.linear[0].affine.center, start.center);
        assert_eq!(out.total.center, start.center);
        let none = StagePlan {
            linear: Vec::new(),
            ..plan.clone()
        };
        let err = register_stages(
            &CpuBackend,
            &CpuLinearEngine,
            &none,
            &fx,
            &mv,
            None,
            &SynOptions::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no stage"), "{err}");
    }

    #[test]
    fn the_preprocessing_window_is_the_plan_s() {
        let (fx, mv) = pair();
        let mut t = RegisterTimings::default();
        let default = crate::register::prepare_channels_full(&fx, &mv, None, &mut t).unwrap();
        let explicit = prepare_channels_with(
            &fx,
            &mv,
            None,
            Some((WINSORIZE_LOWER, WINSORIZE_UPPER)),
            &mut t,
        )
        .unwrap();
        assert_eq!(default.channels[0].fixed, explicit.channels[0].fixed);
        let plain = prepare_channels_with(&fx, &mv, None, None, &mut t).unwrap();
        let direct = crate::preprocess::preprocess_volume_no_winsorize(&fx[0]).unwrap();
        assert_eq!(plain.channels[0].fixed, direct);
        assert_ne!(plain.channels[0].fixed, default.channels[0].fixed);
        for bad in [(0.5, 0.5), (-0.1, 0.9), (0.1, 1.5), (f32::NAN, 0.9)] {
            assert!(prepare_channels_with(&fx, &mv, None, Some(bad), &mut t).is_err());
        }
    }

    #[test]
    fn a_plan_checks_its_stages_and_channels_before_any_work() {
        let (fx, mv) = pair();
        let mut syn = small_syn();
        syn.channel_weights = vec![0.5, 0.5];
        let plan = StagePlan {
            initial: InitialTransform::Identity,
            linear: Vec::new(),
            syn: Some(syn),
            winsorize: None,
        };
        let err = register_stages(
            &CpuBackend,
            &CpuLinearEngine,
            &plan,
            &fx,
            &mv,
            None,
            &SynOptions::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("channel weights"), "{err}");
        let two = StagePlan {
            initial: InitialTransform::Identity,
            linear: vec![small(LinearRecipe::cohort_rigid())],
            syn: None,
            winsorize: None,
        };
        let fx2 = vec![fx[0].clone(), fx[0].clone()];
        let mv2 = vec![mv[0].clone(), mv[0].clone()];
        let err = register_stages(
            &CpuBackend,
            &CpuLinearEngine,
            &two,
            &fx2,
            &mv2,
            None,
            &SynOptions::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("one channel"), "{err}");
        let singular = StagePlan {
            initial: InitialTransform::Affine(AffineTransform::from_offset(
                [[0.0; 3], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [0.0; 3],
            )),
            linear: vec![small(LinearRecipe::cohort_rigid())],
            syn: None,
            winsorize: None,
        };
        assert!(singular.validate().is_err());
    }
}
