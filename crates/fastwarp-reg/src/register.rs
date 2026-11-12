// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The production normalize end to end: isotropic registration grid, per-stage intensity
//! preprocessing, the linear stages ([`register_full`]: centre of mass, `Rigid`, `Affine`),
//! [`run_syn`], and the ANTs-named outputs.
//!
//! [`register`] runs only the SyN stage: the caller supplies the affine that the earlier stages
//! produced (for example element 0 of an ANTs `Composite.h5`).

use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;

use crate::apply::{apply_chain, Interpolation};
use crate::backend::Backend;
use crate::error::{invalid, Result};
use crate::grid::Grid;
use crate::linear::{
    run_linear_chain_cached, LinearChainResult, LinearEngine, LinearRecipe, SmoothCache,
};
use crate::preprocess::preprocess_volume;
use crate::recipe::SynRecipe;
use crate::syn::{run_syn_pair_cached, SynChannel, SynLevelTrace, SynOptions};
use crate::transform::itk_mat::write_mat_file;
use crate::transform::{write_composite_pair, AffineTransform, CompositePair, TransformElem};
use crate::volume::Volume;

/// Lower quantile of the winsorisation of `--winsorize-image-intensities [0.005,0.995]`.
pub const WINSORIZE_LOWER: f32 = 0.005;
/// Upper quantile of the winsorisation.
pub const WINSORIZE_UPPER: f32 = 0.995;

/// Input of [`register`]: one fixed and one moving full-resolution image per channel.
#[derive(Clone, Copy, Debug)]
pub struct RegisterInput<'a> {
    /// Template channels (all on one grid).
    pub fixed: &'a [Volume<f64>],
    /// Moving channels, in the order of `fixed`.
    pub moving: &'a [Volume<f64>],
    /// Linear transform of the earlier stages (fixed points to moving points).
    pub initial: AffineTransform,
    /// Registration grid spacing in millimetres; `None` registers on the images as they are.
    pub iso_mm: Option<f64>,
}

/// Wall-clock seconds per phase of [`register`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RegisterTimings {
    /// Isotropic resampling of all channels.
    pub resample_s: f64,
    /// Winsorisation and rescaling.
    pub preprocess_s: f64,
    /// The linear stages (centre of mass, rigid, affine); zero when the affine is given.
    pub linear_s: f64,
    /// [`run_syn`], including host-side smoothing and device transfers.
    pub syn_s: f64,
    /// Forward warp of the first moving channel onto the registration grid.
    pub warp_s: f64,
}

/// Result of [`register`].
#[derive(Clone, Debug)]
pub struct RegisterOutput {
    /// `Composite = [Affine, Warp]`, `InverseComposite = [InverseWarp, Affine^-1]`.
    pub pair: CompositePair,
    /// The first moving channel through the forward chain onto the registration grid.
    pub warped: Volume<f64>,
    /// The registration grid (grid of both fields).
    pub grid: Grid,
    /// Per-level traces of the SyN loop.
    pub levels: Vec<SynLevelTrace>,
    /// Phase timings.
    pub timings: RegisterTimings,
    /// The linear stages, when they were run here.
    pub linear: Option<LinearChainResult>,
}

fn sample_linear_clamped(data: &[f64], size: [usize; 3], c: [f64; 3]) -> f64 {
    let lo = [c[0].floor(), c[1].floor(), c[2].floor()];
    let fr = [c[0] - lo[0], c[1] - lo[1], c[2] - lo[2]];
    let mut acc = 0.0;
    for corner in 0..8usize {
        let mut idx = [0usize; 3];
        let mut w = 1.0;
        let mut inside = true;
        for a in 0..3 {
            let bit = (corner >> a) & 1;
            let i = lo[a] as i64 + bit as i64;
            if i < 0 || i >= size[a] as i64 {
                inside = false;
                break;
            }
            idx[a] = i as usize;
            w *= if bit == 1 { fr[a] } else { 1.0 - fr[a] };
        }
        if w == 0.0 || !inside {
            continue;
        }
        acc += w * data[idx[0] + size[0] * (idx[1] + size[1] * idx[2])];
    }
    acc
}

/// Resamples `vol` onto an isotropic `mm` grid with the pipeline's rule: same origin and
/// direction, `floor((n - 1) * spacing / mm + 1e-9) + 1` voxels per axis, output voxel `i`
/// samples source index `min(i * mm / spacing, n - 1)` trilinearly, the result is rounded to
/// `f32`. Returns a copy when every axis already has `mm` spacing (within 1e-6 mm).
pub fn resample_isotropic(vol: &Volume<f64>, mm: f64) -> Result<Volume<f64>> {
    if !(mm.is_finite() && mm > 0.0) {
        return Err(invalid("register", "isotropic spacing must be positive"));
    }
    let g = *vol.grid();
    let spacing = g.spacing();
    if spacing.iter().all(|s| (s - mm).abs() < 1e-6) {
        return Ok(vol.clone());
    }
    let size = g.size();
    let mut out_n = [1usize; 3];
    let mut coords: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for a in 0..3 {
        let last = size[a].saturating_sub(1) as f64;
        let ratio = mm / spacing[a];
        let t = (((last * spacing[a] / mm) + 1e-9).floor() as usize + 1).max(1);
        coords[a] = (0..t).map(|i| (i as f64 * ratio).min(last)).collect();
        out_n[a] = t;
    }
    let grid = Grid::new(out_n, [mm; 3], g.origin(), g.direction())?;
    let src = vol.data();
    let data: Vec<f64> = (0..grid.num_voxels())
        .into_par_iter()
        .map(|lin| {
            let [i, j, k] = grid.ijk(lin);
            let v = sample_linear_clamped(src, size, [coords[0][i], coords[1][j], coords[2][k]]);
            f64::from(v as f32)
        })
        .collect();
    Volume::from_vec(grid, data)
}

/// Channels of a registration prepared for the stages: the resampled raw images and the
/// preprocessed SyN channels.
#[derive(Clone, Debug)]
pub struct PreparedChannels {
    /// Preprocessed (winsorised, windowed to `[0, 1]`) fixed and moving image of every channel.
    pub channels: Vec<SynChannel>,
    /// The resampled, un-preprocessed fixed images.
    pub fixed_raw: Vec<Volume<f64>>,
    /// The resampled, un-preprocessed moving images.
    pub moving_raw: Vec<Volume<f64>>,
}

/// [`prepare_channels_full`] returning the preprocessed channels with the resampled, raw moving
/// volumes.
pub fn prepare_channels(
    fixed: &[Volume<f64>],
    moving: &[Volume<f64>],
    iso_mm: Option<f64>,
    timings: &mut RegisterTimings,
) -> Result<(Vec<SynChannel>, Vec<Volume<f64>>)> {
    let p = prepare_channels_full(fixed, moving, iso_mm, timings)?;
    Ok((p.channels, p.moving_raw))
}

/// Resamples every channel to the registration grid (when `iso_mm` is set) and builds the
/// preprocessed [`SynChannel`]s, keeping the resampled raw images.
pub fn prepare_channels_full(
    fixed: &[Volume<f64>],
    moving: &[Volume<f64>],
    iso_mm: Option<f64>,
    timings: &mut RegisterTimings,
) -> Result<PreparedChannels> {
    if fixed.is_empty() || fixed.len() != moving.len() {
        return Err(invalid(
            "register",
            "fixed and moving must have the same, non-zero number of channels",
        ));
    }
    if fixed
        .iter()
        .chain(moving)
        .any(|v| v.data().par_iter().any(|x| !x.is_finite()))
    {
        return Err(invalid(
            "register",
            "an input image has non-finite (NaN or infinite) voxels",
        ));
    }
    let t0 = Instant::now();
    let resample = |v: &Volume<f64>| match iso_mm {
        Some(mm) => resample_isotropic(v, mm),
        None => Ok(v.clone()),
    };
    let fixed_raw = fixed.iter().map(resample).collect::<Result<Vec<_>>>()?;
    let moving_raw = moving.iter().map(resample).collect::<Result<Vec<_>>>()?;
    timings.resample_s += t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let mut channels = Vec::with_capacity(fixed_raw.len());
    for (f, m) in fixed_raw.iter().zip(&moving_raw) {
        channels.push(SynChannel {
            fixed: preprocess_volume(f, WINSORIZE_LOWER, WINSORIZE_UPPER, 0.0, 1.0)?,
            moving: preprocess_volume(m, WINSORIZE_LOWER, WINSORIZE_UPPER, 0.0, 1.0)?,
        });
    }
    timings.preprocess_s += t1.elapsed().as_secs_f64();
    Ok(PreparedChannels {
        channels,
        fixed_raw,
        moving_raw,
    })
}

/// Runs the SyN stage of the cohort recipe on `backend`.
pub fn register<B: Backend>(
    backend: &B,
    recipe: &SynRecipe,
    input: &RegisterInput<'_>,
    options: &SynOptions<'_>,
) -> Result<RegisterOutput> {
    let mut timings = RegisterTimings::default();
    let (channels, mut moving_r) =
        prepare_channels(input.fixed, input.moving, input.iso_mm, &mut timings)?;
    moving_r.truncate(1);
    let t = Instant::now();
    let (pair, levels) =
        run_syn_pair_cached(backend, recipe, &channels, &input.initial, options, None)?;
    timings.syn_s = t.elapsed().as_secs_f64();
    let grid = *channels[0].fixed.grid();
    drop(channels);
    let t = Instant::now();
    let warped = apply_chain(
        &moving_r[0],
        &grid,
        &pair.forward,
        Interpolation::Linear,
        0.0,
    )?;
    timings.warp_s = t.elapsed().as_secs_f64();
    Ok(RegisterOutput {
        pair,
        warped,
        grid,
        levels,
        timings,
        linear: None,
    })
}

/// The linear stages of the cohort recipe.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearPlan {
    /// The `Rigid` stage.
    pub rigid: LinearRecipe,
    /// The `Affine` stage.
    pub affine: LinearRecipe,
}

impl LinearPlan {
    /// `Rigid[0.1]` then `Affine[0.1]`, `MI[...,1,32,Regular,0.25]`, `[1000x500x250x100,1e-6,10]`.
    pub fn cohort() -> Self {
        Self {
            rigid: LinearRecipe::cohort_rigid(),
            affine: LinearRecipe::cohort_affine(),
        }
    }
}

/// The full normalize: centre-of-mass initialisation, `Rigid`, `Affine` (on `engine`, first
/// channel only, as the cohort argv has one `MI` metric per linear stage), then the SyN stage on
/// `backend` starting from the collapsed affine. No external process is involved.
#[allow(clippy::too_many_arguments)]
pub fn register_full<B: Backend, E: LinearEngine>(
    backend: &B,
    engine: &E,
    plan: &LinearPlan,
    recipe: &SynRecipe,
    fixed: &[Volume<f64>],
    moving: &[Volume<f64>],
    iso_mm: Option<f64>,
    options: &SynOptions<'_>,
) -> Result<RegisterOutput> {
    let mut timings = RegisterTimings::default();
    let PreparedChannels {
        channels,
        fixed_raw,
        moving_raw,
    } = prepare_channels_full(fixed, moving, iso_mm, &mut timings)?;
    let t = Instant::now();
    let mut cache = SmoothCache::default();
    let chain = run_linear_chain_cached(
        engine,
        &plan.rigid,
        &plan.affine,
        &fixed_raw[0],
        &moving_raw[0],
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
    let t = Instant::now();
    let syn = run_syn_pair_cached(
        backend,
        recipe,
        &channels,
        &chain.total,
        options,
        Some(&mut cache),
    );
    drop(cache);
    let (pair, levels) = syn?;
    timings.syn_s = t.elapsed().as_secs_f64();
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
    Ok(RegisterOutput {
        pair,
        warped,
        grid,
        levels,
        timings,
        linear: Some(chain),
    })
}

/// Writes `<prefix>0GenericAffine.mat`, the collapsed linear transform (what ANTs writes for
/// `--collapse-output-transforms`).
pub fn write_linear_output(prefix: &Path, total: &AffineTransform) -> Result<()> {
    let name = prefix
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = prefix.with_file_name(format!("{name}0GenericAffine.mat"));
    write_mat_file(&path, &[TransformElem::Affine(*total)])
}

/// Writes `<prefix>Composite.h5`, `<prefix>InverseComposite.h5` and `<prefix>_Warped.nii.gz`.
pub fn write_outputs(prefix: &Path, out: &RegisterOutput) -> Result<()> {
    write_composite_pair(prefix, &out.pair)?;
    let name = prefix
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let warped_path = prefix.with_file_name(format!("{name}_Warped.nii.gz"));
    let f32_data: Vec<f32> = out.warped.data().iter().map(|v| *v as f32).collect();
    Volume::from_vec(*out.warped.grid(), f32_data)?.save_nifti(&warped_path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cpu::CpuBackend;
    use crate::linalg::IDENTITY3;
    use crate::syn::{run_syn, run_syn_cached};

    fn blob_volume(n: usize, spacing: f64, shift: f64) -> Volume<f64> {
        let g = Grid::new([n, n, n], [spacing; 3], [0.0; 3], IDENTITY3).unwrap();
        let c = (n as f64 - 1.0) * spacing / 2.0;
        let data = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let p = g.index_to_point([i as f64, j as f64, k as f64]);
                let r2 = (p[0] - c - shift).powi(2) + (p[1] - c).powi(2) + (p[2] - c).powi(2);
                let noise = ((lin * 2654435761) % 1000) as f64 / 20000.0;
                (-r2 / (2.0 * (0.25 * n as f64 * spacing).powi(2))).exp() + noise
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    }

    #[test]
    fn isotropic_resample_follows_the_pipeline_grid_rule() {
        let v = blob_volume(21, 0.5, 0.0);
        let r = resample_isotropic(&v, 1.0).unwrap();
        assert_eq!(r.grid().size(), [11, 11, 11]);
        assert_eq!(r.grid().spacing(), [1.0; 3]);
        assert_eq!(r.grid().origin(), v.grid().origin());
        let a = r.data()[r.grid().linear_index(3, 4, 5)];
        let b = v.data()[v.grid().linear_index(6, 8, 10)];
        assert!((a - b).abs() < 1e-6);
        let same = resample_isotropic(&r, 1.0).unwrap();
        assert_eq!(same, r);
    }

    #[test]
    fn isotropic_resample_interpolates_between_source_voxels() {
        let g = Grid::new([4, 1, 1], [1.5, 1.0, 1.0], [0.0; 3], IDENTITY3).unwrap();
        let v = Volume::from_vec(g, vec![0.0, 3.0, 6.0, 9.0]).unwrap();
        let r = resample_isotropic(&v, 1.0).unwrap();
        assert_eq!(r.grid().size(), [5, 1, 1]);
        for (i, want) in [0.0, 2.0, 4.0, 6.0, 8.0].iter().enumerate() {
            assert!((r.data()[i] - want).abs() < 1e-6, "{i}");
        }
    }

    #[test]
    fn non_finite_input_voxels_are_rejected() {
        let fixed = blob_volume(8, 2.0, 0.0);
        let mut moving = blob_volume(8, 2.0, 1.0);
        moving.data_mut()[5] = f64::NAN;
        let mut timings = RegisterTimings::default();
        let err = prepare_channels_full(
            std::slice::from_ref(&fixed),
            std::slice::from_ref(&moving),
            None,
            &mut timings,
        )
        .unwrap_err();
        assert!(err.to_string().contains("non-finite"), "{err}");
        moving.data_mut()[5] = 0.0;
        assert!(prepare_channels_full(
            std::slice::from_ref(&fixed),
            std::slice::from_ref(&moving),
            None,
            &mut timings,
        )
        .is_ok());
    }

    #[test]
    fn register_full_runs_the_linear_stages_and_writes_the_collapsed_affine() {
        use crate::linear::{CpuLinearEngine, LinearRecipe};
        let fixed = blob_volume(24, 2.0, 0.0);
        let moving = blob_volume(24, 2.0, 4.0);
        let small = |r: LinearRecipe| LinearRecipe {
            iterations: vec![30, 20],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..r
        };
        let plan = LinearPlan {
            rigid: small(LinearRecipe::cohort_rigid()),
            affine: small(LinearRecipe::cohort_affine()),
        };
        let recipe = SynRecipe {
            iterations: vec![6, 4],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..SynRecipe::cohort()
        };
        let fx = [fixed.clone()];
        let mv = [moving.clone()];
        let out = register_full(
            &CpuBackend,
            &CpuLinearEngine,
            &plan,
            &recipe,
            &fx,
            &mv,
            Some(2.0),
            &SynOptions::default(),
        )
        .unwrap();
        let chain = out.linear.as_ref().unwrap();
        let c = 23.0;
        let mapped = chain.total.apply([c, c, c]);
        assert!((mapped[0] - (c + 4.0)).abs() < 1.2, "{mapped:?}");
        assert!(out.timings.linear_s > 0.0);
        let dir = std::env::temp_dir().join(format!("fastwarp_reg_full_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prefix = dir.join("sub-y_");
        write_outputs(&prefix, &out).unwrap();
        write_linear_output(&prefix, &chain.total).unwrap();
        let mat = dir.join("sub-y_0GenericAffine.mat");
        assert!(mat.is_file());
        let back = crate::transform::read_transform_file(&mat).unwrap();
        match back.queue().first() {
            Some(TransformElem::Affine(a)) => {
                let (x, y) = (
                    a.apply([5.0, 9.0, 11.0]),
                    chain.total.apply([5.0, 9.0, 11.0]),
                );
                assert!((x[0] - y[0]).abs() < 1e-9 && (x[1] - y[1]).abs() < 1e-9);
            }
            _ => panic!("no affine in the written file"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn register_writes_ants_named_outputs_and_reduces_the_mismatch() {
        let fixed = blob_volume(16, 2.0, 0.0);
        let moving = blob_volume(16, 2.0, 3.0);
        let recipe = SynRecipe {
            iterations: vec![8, 6],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..SynRecipe::cohort()
        };
        let fx = [fixed.clone()];
        let mv = [moving.clone()];
        let input = RegisterInput {
            fixed: &fx,
            moving: &mv,
            initial: AffineTransform::identity(),
            iso_mm: Some(2.0),
        };
        let out = register(&CpuBackend, &recipe, &input, &SynOptions::default()).unwrap();
        let dir =
            std::env::temp_dir().join(format!("fastwarp_reg_register_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prefix = dir.join("sub-x_");
        write_outputs(&prefix, &out).unwrap();
        assert!(dir.join("sub-x_Composite.h5").is_file());
        assert!(dir.join("sub-x_InverseComposite.h5").is_file());
        assert!(dir.join("sub-x__Warped.nii.gz").is_file());
        let back = crate::transform::read_composite_pair(&prefix).unwrap();
        assert_eq!(back.forward.len(), 2);
        assert_eq!(back.inverse.len(), 2);
        let err = |a: &Volume<f64>| {
            a.data()
                .iter()
                .zip(fixed.data())
                .map(|(x, y)| (x - y).powi(2))
                .sum::<f64>()
        };
        assert!(err(&out.warped) < err(&moving));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_syn_stage_reuses_the_linear_stage_smoothing_without_changing_a_bit() {
        use crate::linear::{CpuLinearEngine, LinearRecipe};
        let fixed = blob_volume(24, 2.0, 0.0);
        let moving = blob_volume(24, 2.0, 4.0);
        let sigmas = vec![2.0, 1.0, 0.0];
        let small = |r: LinearRecipe| LinearRecipe {
            iterations: vec![6, 4, 3],
            shrink_factors: vec![4, 2, 1],
            smoothing_sigmas: sigmas.clone(),
            ..r
        };
        let recipe = SynRecipe {
            iterations: vec![3, 2, 2],
            shrink_factors: vec![4, 2, 1],
            smoothing_sigmas: sigmas.clone(),
            ..SynRecipe::cohort()
        };
        let mut timings = RegisterTimings::default();
        let prepared = prepare_channels_full(
            std::slice::from_ref(&fixed),
            std::slice::from_ref(&moving),
            None,
            &mut timings,
        )
        .unwrap();
        let mut cache = crate::linear::SmoothCache::default();
        let chain = run_linear_chain_cached(
            &CpuLinearEngine,
            &small(LinearRecipe::cohort_rigid()),
            &small(LinearRecipe::cohort_affine()),
            &prepared.fixed_raw[0],
            &prepared.moving_raw[0],
            &prepared.channels[0].fixed,
            &prepared.channels[0].moving,
            &mut cache,
            None,
        )
        .unwrap();
        let options = SynOptions::default();
        let fresh = run_syn(
            &CpuBackend,
            &recipe,
            &prepared.channels,
            &chain.total,
            &options,
        )
        .unwrap();
        let shared = run_syn_cached(
            &CpuBackend,
            &recipe,
            &prepared.channels,
            &chain.total,
            &options,
            Some(&mut cache),
        )
        .unwrap();
        assert_eq!(fresh.pair.forward, shared.pair.forward);
        assert_eq!(fresh.pair.inverse, shared.pair.inverse);
        assert_eq!(fresh.warp, shared.warp);
        assert!(cache
            .take(
                &prepared.channels[0].fixed,
                &prepared.channels[0].moving,
                2.0,
                crate::pyramid::SigmaUnits::Voxels
            )
            .is_none());
    }

    #[test]
    fn a_set_cancel_flag_stops_the_syn_stage_and_the_full_registration() {
        use crate::error::RegError;
        use crate::linear::{
            run_rigid_registration_cancellable, run_rigid_registration_from_cancellable,
            CpuLinearEngine, LinearRecipe,
        };
        use std::sync::atomic::AtomicBool;
        let fixed = blob_volume(16, 2.0, 0.0);
        let moving = blob_volume(16, 2.0, 3.0);
        let recipe = SynRecipe {
            iterations: vec![8, 6],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..SynRecipe::cohort()
        };
        let set = AtomicBool::new(true);
        let cancel = SynOptions { cancel: Some(&set) };
        let mut timings = RegisterTimings::default();
        let prepared = prepare_channels_full(
            std::slice::from_ref(&fixed),
            std::slice::from_ref(&moving),
            None,
            &mut timings,
        )
        .unwrap();
        let identity = AffineTransform::identity();
        let e = run_syn(&CpuBackend, &recipe, &prepared.channels, &identity, &cancel).unwrap_err();
        assert!(matches!(e, RegError::Cancelled), "{e}");
        let fx = [fixed.clone()];
        let mv = [moving.clone()];
        let input = RegisterInput {
            fixed: &fx,
            moving: &mv,
            initial: identity,
            iso_mm: None,
        };
        let e = register(&CpuBackend, &recipe, &input, &cancel).unwrap_err();
        assert!(matches!(e, RegError::Cancelled), "{e}");
        let e = register_full(
            &CpuBackend,
            &CpuLinearEngine,
            &LinearPlan::cohort(),
            &SynRecipe::cohort(),
            &fx,
            &mv,
            None,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(e, RegError::Cancelled), "{e}");
        let rigid = LinearRecipe::cohort_rigid();
        let e = run_rigid_registration_cancellable(
            &CpuLinearEngine,
            &rigid,
            &fixed,
            &moving,
            Some(&set),
        )
        .unwrap_err();
        assert!(matches!(e, RegError::Cancelled), "{e}");
        let e = run_rigid_registration_from_cancellable(
            &CpuLinearEngine,
            &rigid,
            &fixed,
            &moving,
            identity,
            Some(&set),
        )
        .unwrap_err();
        assert!(matches!(e, RegError::Cancelled), "{e}");
        let unset = AtomicBool::new(false);
        let options = SynOptions {
            cancel: Some(&unset),
        };
        let plain = run_syn(
            &CpuBackend,
            &recipe,
            &prepared.channels,
            &identity,
            &SynOptions::default(),
        )
        .unwrap();
        let polled = run_syn(
            &CpuBackend,
            &recipe,
            &prepared.channels,
            &identity,
            &options,
        )
        .unwrap();
        assert_eq!(plain.warp, polled.warp);
        assert_eq!(plain.inverse_warp, polled.inverse_warp);
    }
}
