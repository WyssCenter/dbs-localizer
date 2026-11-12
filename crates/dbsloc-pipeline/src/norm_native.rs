// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The `ants_syn` normalization on the in-process engine (`--reg-backend wgpu`, `specs/04` §5.4):
//! the same stage flow as the ANTs path, with every registration and apply call made by
//! `fastwarp-reg` and the outputs written under the exact ANTs names.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use rayon::prelude::*;
use serde_json::{json, Map, Value};

use fastwarp_reg::apply::{apply_chain, apply_nifti, Interpolation};
use fastwarp_reg::linear::{LinearRecipe, LinearStageResult};
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::register::{write_linear_output, write_outputs, LinearPlan, RegisterOutput};
use fastwarp_reg::transform::itk_mat::write_mat_file;
use fastwarp_reg::transform::{
    read_composite_pair, read_transform_file, AffineTransform, CompositePair, TransformChain,
    TransformElem,
};
use fastwarp_reg::Volume;

use crate::error::{PipelineError, PipelineResult};
use crate::protocol::RunEvent;
use crate::reg_backend::{cancelled_or, RegEngine};
use crate::stage_impl::{
    file_name_of, load_reg_volume, load_reg_volume_pair, log, NormChannels, NORM_T2_CHANNEL,
};

pub(crate) struct NativeApply {
    pub key: String,
    pub src: PathBuf,
    pub tmp_out: PathBuf,
}

pub(crate) struct NativeNormalize<'a> {
    pub engine: &'a dyn RegEngine,
    pub moving: &'a Path,
    pub template: &'a Path,
    pub channels: &'a NormChannels,
    pub iso_mm: Option<f64>,
    pub threads: Option<u32>,
    pub apply: &'a [NativeApply],
    pub tmp_prefix: &'a Path,
    pub cancel: Option<&'a AtomicBool>,
}

fn cancelled(cancel: Option<&AtomicBool>) -> PipelineResult<()> {
    if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        Err(PipelineError::Cancelled)
    } else {
        Ok(())
    }
}

fn reg_failed(engine: &dyn RegEngine, what: &str, e: &dyn std::fmt::Display) -> PipelineError {
    reg_failed_in("normalize", engine, what, e)
}

fn reg_failed_in(
    stage: &str,
    engine: &dyn RegEngine,
    what: &str,
    e: &dyn std::fmt::Display,
) -> PipelineError {
    let text = e.to_string();
    let hint = if text.contains("out of memory") {
        "; retry with a coarser --norm-isotropic-mm or --reg-backend ants"
    } else if text.contains("binds at most") {
        "; use --norm-isotropic-mm 1.0 or --reg-backend ants"
    } else {
        ""
    };
    PipelineError::Message(format!(
        "{stage}: {what} failed on {}: {text}{hint}",
        engine.label()
    ))
}

/// Test/debug input: reuse an external T2w → anchor
/// rigid instead of running the GPU rigid, then continue with the SyN stages. Points at an ITK
/// affine `.mat`; only the first element is read.
pub const T2_COREG_FROM_ENV: &str = "DBSLOC_T2_COREG_FROM";

fn injected_t2_rigid() -> PipelineResult<Option<(PathBuf, AffineTransform)>> {
    parse_injected_t2(std::env::var_os(T2_COREG_FROM_ENV))
}

fn parse_injected_t2(
    raw: Option<std::ffi::OsString>,
) -> PipelineResult<Option<(PathBuf, AffineTransform)>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(raw);
    let chain = read_transform_file(&path).map_err(|e| {
        PipelineError::Message(format!(
            "normalize: cannot read {T2_COREG_FROM_ENV} {}: {e}",
            path.display()
        ))
    })?;
    match chain.queue().first() {
        Some(TransformElem::Affine(a)) => Ok(Some((path, *a))),
        _ => Err(PipelineError::Message(format!(
            "normalize: {T2_COREG_FROM_ENV} {} has no affine as its first element",
            path.display()
        ))),
    }
}

/// The linear part of a forward composite `[L1, …, Lk, W]`: `L1 ∘ … ∘ Lk`. Like the composite it
/// maps template LPS points to native LPS points. A single affine element is returned unchanged
/// (same centre), so its `.mat` holds exactly the parameters of the composite.
pub(crate) fn forward_linear_part(chain: &TransformChain) -> Result<AffineTransform, String> {
    let queue = chain.queue();
    let linear: Vec<&AffineTransform> = queue
        .iter()
        .map_while(|e| match e {
            TransformElem::Affine(a) => Some(a),
            TransformElem::DisplacementField(_) => None,
        })
        .collect();
    if queue[linear.len()..]
        .iter()
        .any(|e| matches!(e, TransformElem::Affine(_)))
    {
        return Err(
            "the forward composite applies an affine before its displacement field, so its linear \
             part cannot be separated"
                .into(),
        );
    }
    let mut parts = linear.into_iter();
    let first = parts
        .next()
        .ok_or_else(|| "the forward composite holds no affine transform".to_owned())?;
    Ok(parts.fold(*first, |total, next| AffineTransform::compose(&total, next)))
}

pub(crate) fn read_forward_linear_part(composite: &Path) -> PipelineResult<AffineTransform> {
    let chain = read_transform_file(composite).map_err(|e| {
        PipelineError::Message(format!(
            "normalize: cannot read the forward composite {}: {e}",
            file_name_of(composite)
        ))
    })?;
    forward_linear_part(&chain).map_err(|e| {
        PipelineError::Message(format!("normalize: {e} ({})", file_name_of(composite)))
    })
}

/// Writes `forward` and its inverse as ITK `.mat` files; returns the inverse.
pub(crate) fn write_norm_affines(
    forward: &AffineTransform,
    forward_path: &Path,
    inverse_path: &Path,
) -> PipelineResult<AffineTransform> {
    let inverse = forward
        .inverse()
        .map_err(|e| PipelineError::Message(format!("normalize: the normalization affine: {e}")))?;
    for (path, a) in [(forward_path, forward), (inverse_path, &inverse)] {
        write_mat_file(path, &[TransformElem::Affine(*a)]).map_err(|e| {
            PipelineError::Message(format!("normalize: {}: {e}", file_name_of(path)))
        })?;
    }
    Ok(inverse)
}

/// `a` (LPS mm, ITK point map) as a homogeneous RAS mm matrix: `p' = R·[p; 1]` for RAS points.
pub(crate) fn ras_4x4(a: &AffineTransform) -> [[f64; 4]; 4] {
    const FLIP: [f64; 3] = [-1.0, -1.0, 1.0];
    let offset = a.offset();
    let mut out = [[0.0; 4]; 4];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = FLIP[r] * a.matrix[r][c] * FLIP[c];
        }
        out[r][3] = FLIP[r] * offset[r];
    }
    out[3][3] = 1.0;
    out
}

fn rounded_to_f32(v: &Volume<f64>) -> PipelineResult<(Volume<f64>, Volume<f32>)> {
    let f32_data: Vec<f32> = v.data().iter().map(|x| *x as f32).collect();
    let f64_data: Vec<f64> = f32_data.iter().map(|x| f64::from(*x)).collect();
    let as_f64 = Volume::from_vec(*v.grid(), f64_data)
        .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;
    let as_f32 = Volume::from_vec(*v.grid(), f32_data)
        .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;
    Ok((as_f64, as_f32))
}

fn stage_levels(stage: &LinearStageResult) -> Value {
    json!(stage
        .levels
        .iter()
        .map(|l| json!({
            "level": l.level,
            "grid": l.grid_size,
            "samples": l.samples,
            "iterations": l.cg.iterations,
            "converged": l.cg.converged,
            "best_value": l.cg.best_value,
            "seconds": l.seconds,
        }))
        .collect::<Vec<_>>())
}

fn syn_levels(out: &RegisterOutput) -> Value {
    json!(out
        .levels
        .iter()
        .map(|l| json!({
            "level": l.level,
            "grid": l.grid_size,
            "iterations": l.iterations.len(),
            "converged": l.converged,
            "last_energy": l.iterations.last().map(|i| i.energy),
        }))
        .collect::<Vec<_>>())
}

fn affine_chain(total: &AffineTransform) -> TransformChain {
    TransformChain::from_queue(vec![TransformElem::Affine(*total)])
}

pub(crate) struct PairJob<'a> {
    pub engine: &'a dyn RegEngine,
    pub stage: &'static str,
    pub iso_mm: Option<f64>,
    pub tmp_prefix: &'a Path,
    pub cancel: Option<&'a AtomicBool>,
}

fn register_and_write(
    job: &PairJob<'_>,
    fixed: Vec<Volume<f64>>,
    moving: Vec<Volume<f64>>,
    tx: Option<&mpsc::Sender<RunEvent>>,
    extra: &mut Map<String, Value>,
    timings: &mut Map<String, Value>,
) -> PipelineResult<CompositePair> {
    let engine = job.engine;
    let stage = job.stage;
    cancelled(job.cancel)?;
    let recipe = if fixed.len() > 1 {
        SynRecipe::cohort_multispectral()
    } else {
        SynRecipe::cohort()
    };
    let t = Instant::now();
    let out = engine
        .register_cancellable(
            &LinearPlan::cohort(),
            &recipe,
            &fixed,
            &moving,
            job.iso_mm,
            job.cancel,
        )
        .map_err(|e| cancelled_or(e, |e| reg_failed_in(stage, engine, "the registration", e)))?;
    let reg_s = t.elapsed().as_secs_f64();
    let tm = out.timings;
    log(
        tx,
        format!(
            "[{stage}] registered on a {:?} grid in {reg_s:.1} s (linear {:.1} s, SyN {:.1} s)",
            out.grid.size(),
            tm.linear_s,
            tm.syn_s
        ),
    );
    drop((fixed, moving));
    timings.insert("resample_s".into(), json!(tm.resample_s));
    timings.insert("preprocess_s".into(), json!(tm.preprocess_s));
    timings.insert("linear_s".into(), json!(tm.linear_s));
    timings.insert("syn_s".into(), json!(tm.syn_s));
    timings.insert("warp_s".into(), json!(tm.warp_s));
    extra.insert("reg_syn_levels".into(), syn_levels(&out));
    if let Some(chain) = &out.linear {
        extra.insert("reg_rigid_levels".into(), stage_levels(&chain.rigid));
        extra.insert("reg_affine_levels".into(), stage_levels(&chain.affine));
        extra.insert(
            "reg_affine_parameters".into(),
            json!(chain.total.parameters()),
        );
    }
    let t = Instant::now();
    write_outputs(job.tmp_prefix, &out)
        .map_err(|e| PipelineError::Message(format!("{stage}: {e}")))?;
    drop(out);
    timings.insert("write_s".into(), json!(t.elapsed().as_secs_f64()));
    let pair = read_composite_pair(job.tmp_prefix).map_err(|e| {
        PipelineError::Message(format!(
            "{stage}: the composite just written cannot be read back: {e}"
        ))
    })?;
    if pair.forward.len() != 2 || pair.inverse.len() != 2 {
        return Err(PipelineError::Message(format!(
            "{stage}: the composite just written does not hold an affine and a warp"
        )));
    }
    Ok(pair)
}

pub(crate) fn register_templates(
    job: &PairJob<'_>,
    fixed: &[&Path],
    moving: &[&Path],
    threads: Option<u32>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<Map<String, Value>> {
    crate::reg_backend::with_worker_threads(threads, || {
        let total_start = Instant::now();
        let mut extra = Map::new();
        let mut timings = Map::new();
        log(
            tx,
            format!(
                "[{}] registration backend wgpu on {}",
                job.stage,
                job.engine.label()
            ),
        );
        let wanted: Vec<(&Path, &str)> = fixed
            .iter()
            .map(|p| (*p, "fixed template"))
            .chain(moving.iter().map(|p| (*p, "moving template")))
            .collect();
        let loaded: Vec<PipelineResult<Volume<f64>>> = wanted
            .par_iter()
            .map(|(p, what)| load_reg_volume(p, what, job.stage))
            .collect();
        let mut fixed = loaded.into_iter().collect::<PipelineResult<Vec<_>>>()?;
        let moving = fixed.split_off(wanted.len() - moving.len());
        register_and_write(job, fixed, moving, tx, &mut extra, &mut timings)?;
        timings.insert("total_s".into(), json!(total_start.elapsed().as_secs_f64()));
        extra.insert("reg_timings".into(), Value::Object(timings));
        Ok(extra)
    })
    .map_err(|e| PipelineError::Message(format!("{}: {e}", job.stage)))?
}

/// Returns the method-log extras and the linear part of the forward composite.
pub(crate) fn run(
    job: &NativeNormalize<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<(Map<String, Value>, AffineTransform)> {
    crate::reg_backend::with_worker_threads(job.threads, || run_inner(job, tx))
        .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?
}

fn run_inner(
    job: &NativeNormalize<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<(Map<String, Value>, AffineTransform)> {
    let engine = job.engine;
    let total_start = Instant::now();
    let mut extra = Map::new();
    let mut timings = Map::new();
    log(
        tx,
        format!(
            "[normalize] registration backend wgpu on {}",
            engine.label()
        ),
    );
    let t = Instant::now();
    let (anchor, template) = load_reg_volume_pair(
        (job.moving, "anchor image"),
        (job.template, "template"),
        "normalize",
    )?;
    let mut fixed = vec![template];
    let mut moving = vec![anchor];
    timings.insert("load_s".into(), json!(t.elapsed().as_secs_f64()));

    match job.channels {
        NormChannels::Multispectral(t2) => {
            cancelled(job.cancel)?;
            log(
                tx,
                format!(
                    "[normalize] rigid {} → {} ({NORM_T2_CHANNEL} channel, GPU)",
                    t2.source.display(),
                    job.moving.display()
                ),
            );
            let t = Instant::now();
            let (t2_volume, t2_template) = load_reg_volume_pair(
                (&t2.source, "T2w image"),
                (&t2.template, "T2 template"),
                "normalize",
            )?;
            let injected = injected_t2_rigid()?;
            let total = match &injected {
                Some((path, affine)) => {
                    log(
                        tx,
                        format!(
                            "[normalize] reusing the {NORM_T2_CHANNEL} → anchor rigid from {} \
                             ({T2_COREG_FROM_ENV}, test input)",
                            path.display()
                        ),
                    );
                    *affine
                }
                None => {
                    let rigid = engine
                        .rigid_cancellable(
                            &LinearRecipe::cohort_rigid(),
                            &moving[0],
                            &t2_volume,
                            job.cancel,
                        )
                        .map_err(|e| {
                            cancelled_or(e, |e| {
                                reg_failed(engine, "the T2w to anchor rigid registration", e)
                            })
                        })?;
                    extra.insert("reg_t2_coreg_levels".into(), stage_levels(&rigid.rigid));
                    rigid.total
                }
            };
            let warped = apply_chain(
                &t2_volume,
                moving[0].grid(),
                &affine_chain(&total),
                Interpolation::Linear,
                0.0,
            )
            .map_err(|e| reg_failed(engine, "resampling the T2w image", &e))?;
            drop(t2_volume);
            let (warped_f64, warped_f32) = rounded_to_f32(&warped)?;
            drop(warped);
            write_linear_output(&t2.work_prefix, &total)
                .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;
            warped_f32
                .save_nifti(&crate::derivatives::suffixed(
                    &t2.work_prefix,
                    crate::derivatives::ANTS_WARPED_SUFFIX,
                ))
                .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;
            let secs = t.elapsed().as_secs_f64();
            log(
                tx,
                format!("[normalize] {NORM_T2_CHANNEL} → anchor rigid registration: {secs:.1} s"),
            );
            timings.insert("t2_coreg_s".into(), json!(secs));
            extra.insert(
                "reg_t2_coreg".into(),
                json!(if injected.is_some() {
                    "injected"
                } else {
                    "wgpu"
                }),
            );
            if let Some((path, _)) = &injected {
                extra.insert("reg_t2_coreg_from".into(), json!(path.to_string_lossy()));
            }
            fixed.push(t2_template);
            moving.push(warped_f64);
        }
        NormChannels::T1Only(_) => {
            extra.insert("reg_t2_coreg".into(), Value::Null);
        }
    }

    let pair = register_and_write(
        &PairJob {
            engine,
            stage: "normalize",
            iso_mm: job.iso_mm,
            tmp_prefix: job.tmp_prefix,
            cancel: job.cancel,
        },
        fixed,
        moving,
        tx,
        &mut extra,
        &mut timings,
    )?;
    let linear = forward_linear_part(&pair.forward)
        .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;

    for item in job.apply {
        cancelled(job.cancel)?;
        log(
            tx,
            format!(
                "[normalize] applying the composite to {} → {}",
                item.key,
                file_name_of(&item.tmp_out).trim_start_matches(crate::outputs::PARTIAL_PREFIX)
            ),
        );
        let t = Instant::now();
        apply_nifti(
            &item.src,
            job.template,
            &pair.forward,
            Interpolation::Linear,
            0.0,
            &item.tmp_out,
        )
        .map_err(|e| {
            reg_failed(
                engine,
                &format!("applying the composite to {}", item.key),
                &e,
            )
        })?;
        let timing = if item.key == "CT" {
            "ct_apply_s".to_owned()
        } else {
            format!("{}_apply_s", item.key)
        };
        timings.insert(timing, json!(t.elapsed().as_secs_f64()));
    }
    timings.insert("total_s".into(), json!(total_start.elapsed().as_secs_f64()));
    extra.insert("reg_timings".into(), Value::Object(timings));
    Ok((extra, linear))
}

#[cfg(test)]
mod tests {
    use super::{parse_injected_t2, T2_COREG_FROM_ENV};
    use crate::coreg_multistart::write_itk_affine_text;

    #[test]
    fn injected_t2_rigid_reads_the_first_affine_of_a_transform_file() {
        let dir = std::env::temp_dir().join(format!("dbsloc_t2_inject_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t2_to_anchor.txt");
        write_itk_affine_text(
            &path,
            &[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [1.0, 2.0, 3.0],
            [4.0, 5.0, 6.0],
        )
        .unwrap();
        let parsed = parse_injected_t2(Some(path.clone().into_os_string()))
            .unwrap()
            .expect("a parsed affine");
        assert_eq!(parsed.0, path);
        assert_eq!(parsed.1.fixed_parameters(), [1.0, 2.0, 3.0]);
        assert!(parse_injected_t2(None).unwrap().is_none());
        let e = parse_injected_t2(Some(dir.join("missing.mat").into_os_string())).unwrap_err();
        assert!(e.to_string().contains(T2_COREG_FROM_ENV), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::{forward_linear_part, ras_4x4, read_forward_linear_part, write_norm_affines};
    use crate::point_warp::{PointWarp, RustPointWarp};
    use fastwarp_reg::grid::Grid;
    use fastwarp_reg::linalg::IDENTITY3;
    use fastwarp_reg::register::{write_outputs, RegisterOutput, RegisterTimings};
    use fastwarp_reg::transform::{
        read_transform_file, AffineTransform, CompositePair, DisplacementField, TransformChain,
        TransformElem,
    };
    use fastwarp_reg::Volume;

    fn known_affine() -> AffineTransform {
        AffineTransform::new(
            [
                [1.05, 0.04, -0.02],
                [-0.03, 0.97, 0.06],
                [0.01, -0.05, 1.02],
            ],
            [3.5, -7.25, 11.0],
            [-1.5, 12.0, 4.0],
        )
    }

    fn write_known_composite(dir: &std::path::Path) -> std::path::PathBuf {
        let grid = Grid::new([12, 12, 12], [8.0; 3], [-48.0, -48.0, -48.0], IDENTITY3).unwrap();
        let out = RegisterOutput {
            pair: CompositePair::new(
                &known_affine(),
                DisplacementField::zeros(grid),
                DisplacementField::zeros(grid),
            )
            .unwrap(),
            warped: Volume::from_vec(grid, vec![0.0; grid.num_voxels()]).unwrap(),
            grid,
            levels: Vec::new(),
            timings: RegisterTimings::default(),
            linear: None,
        };
        let prefix = dir.join("norm_");
        write_outputs(&prefix, &out).unwrap();
        prefix
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("dbsloc_norm_affine_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_affine_of_a_written_composite_is_extracted_and_inverted() {
        let d = scratch("extract");
        write_known_composite(&d);
        let composite = d.join("norm_Composite.h5");
        let linear = read_forward_linear_part(&composite).unwrap();
        assert_eq!(linear, known_affine());
        let (fwd, inv) = (d.join("fwd.mat"), d.join("inv.mat"));
        let inverse = write_norm_affines(&linear, &fwd, &inv).unwrap();
        assert_eq!(
            read_transform_file(&fwd).unwrap().queue(),
            &[TransformElem::Affine(known_affine())]
        );
        assert_eq!(
            read_transform_file(&inv).unwrap().queue(),
            &[TransformElem::Affine(inverse)]
        );
        for p in [[0.0, 0.0, 0.0], [12.5, -30.0, 7.25], [-40.0, 22.0, -18.5]] {
            let back = inverse.apply(linear.apply(p));
            let there = linear.apply(inverse.apply(p));
            for a in 0..3 {
                assert!((back[a] - p[a]).abs() < 1e-9, "{p:?} -> {back:?}");
                assert!((there[a] - p[a]).abs() < 1e-9, "{p:?} -> {there:?}");
            }
        }
        let warp = RustPointWarp::from_file(&d.join("norm_InverseComposite.h5")).unwrap();
        let m = ras_4x4(&inverse);
        let pts = [[1.0, 2.0, 3.0], [-20.5, 14.25, -9.0], [17.0, -25.0, 30.5]];
        let routed = warp.to_template(&pts).unwrap();
        for (p, r) in pts.iter().zip(&routed) {
            for a in 0..3 {
                let want = m[a][0] * p[0] + m[a][1] * p[1] + m[a][2] * p[2] + m[a][3];
                assert!((want - r[a]).abs() < 1e-4, "{p:?}: {want} vs {}", r[a]);
            }
        }
        assert_eq!(m[3], [0.0, 0.0, 0.0, 1.0]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn several_linear_elements_compose_and_a_misplaced_or_missing_affine_fails() {
        let a = known_affine();
        let b = AffineTransform::new(IDENTITY3, [1.0, -2.0, 0.5], [0.0; 3]);
        let grid = Grid::new([2, 2, 2], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let field = TransformElem::DisplacementField(DisplacementField::zeros(grid));
        let chain = TransformChain::from_queue(vec![
            TransformElem::Affine(a),
            TransformElem::Affine(b),
            field.clone(),
        ]);
        let total = forward_linear_part(&chain).unwrap();
        let p = [4.0, -5.0, 6.0];
        let want = a.apply(b.apply(p));
        let got = total.apply(p);
        for i in 0..3 {
            assert!((want[i] - got[i]).abs() < 1e-12);
        }
        let swapped = TransformChain::from_queue(vec![field.clone(), TransformElem::Affine(a)]);
        assert!(forward_linear_part(&swapped)
            .unwrap_err()
            .contains("cannot be separated"));
        let none = TransformChain::from_queue(vec![field]);
        assert!(forward_linear_part(&none)
            .unwrap_err()
            .contains("no affine"));
    }
}
