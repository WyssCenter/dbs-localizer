// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Instant;

use serde_json::{json, Map, Value};

use fastwarp_core::{mat4_mul, Mat4};
use fastwarp_reg::linear::LinearRecipe;
use fastwarp_reg::nifti_io::header_from_grid;
use fastwarp_reg::register::write_linear_output;
use fastwarp_reg::transform::{read_transform_file, AffineTransform, TransformElem};
use fastwarp_reg::{Grid, Volume};

use crate::ants_argv::build_ants_precoreg_argv;
use crate::bids::Subject;
use crate::derivatives;
use crate::error::{PipelineError, PipelineResult};
use crate::options::RunOptions;
use crate::outputs::{partial_of, Staged};
use crate::protocol::{RunEvent, StageOutcome};
use crate::reg_backend::{cancelled_or, RegBackend, RegChoice, RegEngine, Resolution};
use crate::stage_impl::{
    command_from, file_name_of, load_reg_volume_pair, log, require_3d, require_binary,
    spawn_logged, stage_lock,
};

pub const PRECOREG_STAGE: &str = "precoreg";

const NIFTI1_HEADER_SIZE: usize = 348;
const OFF_PIXDIM: usize = 76;
const OFF_XYZT_UNITS: usize = 123;
const OFF_QFORM_CODE: usize = 252;
const OFF_SFORM_CODE: usize = 254;
const OFF_QUATERN: usize = 256;
const OFF_QOFFSET: usize = 268;
const OFF_SROW: usize = 280;

type ToolExec<'a> = dyn FnMut(&[String]) -> PipelineResult<()> + 'a;

pub fn precoreg_recipe() -> LinearRecipe {
    LinearRecipe {
        iterations: vec![1000, 500, 250],
        shrink_factors: vec![4, 2, 1],
        smoothing_sigmas: vec![2.0, 1.0, 0.0],
        ..LinearRecipe::cohort_rigid()
    }
}

fn lps_flip(m: &Mat4) -> Mat4 {
    let flip = [
        [-1.0, 0.0, 0.0, 0.0],
        [0.0, -1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    mat4_mul(&flip, &mat4_mul(m, &flip))
}

fn affine_as_mat4(t: &AffineTransform) -> Mat4 {
    let origin = t.apply([0.0; 3]);
    let mut m = [[0.0; 4]; 4];
    for axis in 0..3 {
        let mut e = [0.0; 3];
        e[axis] = 1.0;
        let p = t.apply(e);
        for row in 0..3 {
            m[row][axis] = p[row] - origin[row];
        }
    }
    for row in 0..3 {
        m[row][3] = origin[row];
    }
    m[3][3] = 1.0;
    m
}

pub fn ras_map_moving_to_fixed(fixed_to_moving_lps: &AffineTransform) -> PipelineResult<Mat4> {
    let inverse = fixed_to_moving_lps
        .inverse()
        .map_err(|e| PipelineError::Message(format!("precoreg: {e}")))?;
    Ok(lps_flip(&affine_as_mat4(&inverse)))
}

pub fn reheadered_affine(a: &Mat4, m: &Mat4) -> Mat4 {
    mat4_mul(m, a)
}

pub fn rigid_summary(m: &Mat4) -> ([f64; 3], f64, [f64; 3]) {
    let r = |i: usize, j: usize| m[i][j];
    let ry = (-r(2, 0)).clamp(-1.0, 1.0).asin();
    let rx = r(2, 1).atan2(r(2, 2));
    let rz = r(1, 0).atan2(r(0, 0));
    let trace = r(0, 0) + r(1, 1) + r(2, 2);
    let angle = ((trace - 1.0) / 2.0).clamp(-1.0, 1.0).acos();
    (
        [rx.to_degrees(), ry.to_degrees(), rz.to_degrees()],
        angle.to_degrees(),
        [m[0][3], m[1][3], m[2][3]],
    )
}

fn matrix_json(m: &Mat4) -> Value {
    json!(m.iter().map(|row| row.to_vec()).collect::<Vec<_>>())
}

fn get_u16(raw: &[u8], at: usize, little: bool) -> u16 {
    let b = [raw[at], raw[at + 1]];
    if little {
        u16::from_le_bytes(b)
    } else {
        u16::from_be_bytes(b)
    }
}

fn put_f32(raw: &mut [u8], at: usize, v: f32, little: bool) {
    let b = if little {
        v.to_le_bytes()
    } else {
        v.to_be_bytes()
    };
    raw[at..at + 4].copy_from_slice(&b);
}

fn put_u16(raw: &mut [u8], at: usize, v: u16, little: bool) {
    let b = if little {
        v.to_le_bytes()
    } else {
        v.to_be_bytes()
    };
    raw[at..at + 2].copy_from_slice(&b);
}

pub fn rewrite_nifti_affine(raw: &[u8], affine: &Mat4) -> Result<Vec<u8>, String> {
    if raw.len() < NIFTI1_HEADER_SIZE {
        return Err("the file is shorter than a NIfTI-1 header".into());
    }
    let little = if i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) == 348 {
        true
    } else if i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) == 348 {
        false
    } else {
        return Err("not a NIfTI-1 file (header size is not 348)".into());
    };
    let units = raw[OFF_XYZT_UNITS] & 7;
    if units != 0 && units != 2 {
        return Err(format!(
            "unsupported spatial unit code {units} (only millimetres are rewritten)"
        ));
    }
    let size = [
        usize::from(get_u16(raw, 42, little)).max(1),
        usize::from(get_u16(raw, 44, little)).max(1),
        usize::from(get_u16(raw, 46, little)).max(1),
    ];
    let grid = Grid::from_ras_affine(size, affine).map_err(|e| e.to_string())?;
    let h = header_from_grid(&grid);
    let mut out = raw.to_vec();
    put_f32(&mut out, OFF_PIXDIM, h.pixdim[0], little);
    for (i, v) in [h.quatern_b, h.quatern_c, h.quatern_d]
        .into_iter()
        .enumerate()
    {
        put_f32(&mut out, OFF_QUATERN + 4 * i, v, little);
    }
    for (i, v) in [h.quatern_x, h.quatern_y, h.quatern_z]
        .into_iter()
        .enumerate()
    {
        put_f32(&mut out, OFF_QOFFSET + 4 * i, v, little);
    }
    for (r, row) in [h.srow_x, h.srow_y, h.srow_z].into_iter().enumerate() {
        for (c, v) in row.into_iter().enumerate() {
            put_f32(&mut out, OFF_SROW + 16 * r + 4 * c, v, little);
        }
    }
    for at in [OFF_QFORM_CODE, OFF_SFORM_CODE] {
        if get_u16(&out, at, little) == 0 {
            put_u16(&mut out, at, 1, little);
        }
    }
    Ok(out)
}

fn read_nifti_bytes(path: &Path) -> PipelineResult<Vec<u8>> {
    let bytes = std::fs::read(path).map_err(|e| {
        PipelineError::Message(format!("precoreg: cannot read {}: {e}", path.display()))
    })?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut out)
            .map_err(|e| {
                PipelineError::Message(format!(
                    "precoreg: cannot decompress {}: {e}",
                    path.display()
                ))
            })?;
        Ok(out)
    } else {
        Ok(bytes)
    }
}

fn gzip_bytes(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    fastwarp_niftiio::gzip_parallel(raw, 1)
}

pub fn write_reheadered(moving: &Path, affine: &Mat4, out: &Path) -> PipelineResult<()> {
    let raw = read_nifti_bytes(moving)?;
    let patched = rewrite_nifti_affine(&raw, affine)
        .map_err(|e| PipelineError::Message(format!("precoreg: {}: {e}", moving.display())))?;
    let packed = gzip_bytes(&patched)?;
    std::fs::write(out, packed)?;
    Ok(())
}

fn first_affine(path: &Path) -> PipelineResult<AffineTransform> {
    let chain = read_transform_file(path).map_err(|e| {
        PipelineError::Message(format!("precoreg: cannot read {}: {e}", path.display()))
    })?;
    match chain.queue().first() {
        Some(TransformElem::Affine(a)) => Ok(*a),
        _ => Err(PipelineError::Message(format!(
            "precoreg: {} holds no affine transform",
            path.display()
        ))),
    }
}

fn native_rigid(
    engine: &dyn RegEngine,
    fixed: &Path,
    moving: &Path,
    matrix_prefix_tmp: &Path,
    threads: Option<u32>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<(AffineTransform, Value)> {
    log(
        tx,
        format!("[precoreg] registration backend wgpu on {}", engine.label()),
    );
    let started = Instant::now();
    let work = || -> PipelineResult<(AffineTransform, Value)> {
        let (fixed_vol, moving_vol) = load_reg_volume_pair(
            (fixed, "template"),
            (moving, "anchor image"),
            PRECOREG_STAGE,
        )?;
        let out = engine
            .rigid_cancellable(&precoreg_recipe(), &fixed_vol, &moving_vol, cancel)
            .map_err(|e| {
                cancelled_or(e, |e| {
                    PipelineError::Message(format!(
                        "precoreg: the rigid registration failed on {}: {e}",
                        engine.label()
                    ))
                })
            })?;
        write_linear_output(matrix_prefix_tmp, &out.total)
            .map_err(|e| PipelineError::Message(format!("precoreg: {e}")))?;
        Ok((
            out.total,
            json!({"total_s": started.elapsed().as_secs_f64()}),
        ))
    };
    crate::reg_backend::with_worker_threads(threads, work)
        .map_err(|e| PipelineError::Message(format!("precoreg: {e}")))?
}

fn reg_fields(reg: &RegChoice, timings: Option<Value>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("reg_backend".into(), json!(reg.resolution.as_str()));
    m.insert("reg_requested".into(), json!(reg.requested.as_str()));
    if let Resolution::Ants {
        reason: Some(reason),
    } = &reg.resolution
    {
        m.insert("reg_note".into(), json!(reason));
    }
    if let Some(engine) = reg.engine.as_deref() {
        m.insert("reg_adapter".into(), engine.describe());
    }
    if let Some(t) = timings {
        m.insert("reg_timings".into(), t);
    }
    m
}

pub fn run_precoreg(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let reg = crate::reg_backend::choose_for_run(opts.backend_for(crate::stages::Stage::Precoreg))
        .map_err(|e| PipelineError::Message(format!("precoreg: {e}")))?;
    let mut exec = |argv: &[String]| {
        let mut cmd = command_from(argv, opts);
        spawn_logged(&mut cmd, PRECOREG_STAGE, cancel, tx)
    };
    precoreg_with_cancel(
        sub,
        opts,
        reg,
        &|| require_binary("antsRegistration", PRECOREG_STAGE),
        &mut exec,
        cancel,
        tx,
    )
}

#[cfg(test)]
pub(crate) fn precoreg_with(
    sub: &Subject,
    opts: &RunOptions,
    reg: RegChoice,
    find_ants: &dyn Fn() -> PipelineResult<PathBuf>,
    exec: &mut ToolExec<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    precoreg_with_cancel(sub, opts, reg, find_ants, exec, None, tx)
}

pub(crate) fn precoreg_with_cancel(
    sub: &Subject,
    opts: &RunOptions,
    mut reg: RegChoice,
    find_ants: &dyn Fn() -> PipelineResult<PathBuf>,
    exec: &mut ToolExec<'_>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let template = opts
        .normalization_template
        .as_deref()
        .filter(|t| t.is_file())
        .ok_or_else(|| {
            PipelineError::Message(format!(
                "precoreg: {}",
                crate::templates::missing_template_message(
                    opts.normalization_template.as_deref(),
                    None
                )
            ))
        })?;
    let moving = sub
        .anchor_best()
        .filter(|p| p.is_file())
        .ok_or_else(|| PipelineError::Message("no anchor anat".into()))?;
    require_3d(moving, PRECOREG_STAGE)?;
    require_3d(template, PRECOREG_STAGE)?;

    let space = crate::template_catalog::infer_space(template);
    let image = derivatives::precoreg_image(sub);
    let matrix = derivatives::precoreg_transform(sub, &space);
    let record = derivatives::precoreg_record(sub);
    for parent in [&image, &matrix, &record].iter().filter_map(|p| p.parent()) {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_prefix = derivatives::work_prefix(&derivatives::coreg_transform_dir(sub), "pre");
    let staged = Staged::mapped(vec![
        (image.clone(), partial_of(&image)),
        (
            matrix.clone(),
            derivatives::suffixed(&tmp_prefix, derivatives::ANTS_AFFINE_SUFFIX),
        ),
        (record.clone(), partial_of(&record)),
    ])?;

    let res = (|| -> PipelineResult<()> {
        let native = reg.engine.as_deref().map(|engine| {
            native_rigid(
                engine,
                template,
                moving,
                &tmp_prefix,
                opts.ants_max_threads,
                cancel,
                tx,
            )
        });
        let (total, timings) = match native {
            Some(Ok((total, timings))) => (total, Some(timings)),
            Some(Err(PipelineError::Cancelled)) => return Err(PipelineError::Cancelled),
            Some(Err(e)) if reg.requested == RegBackend::Auto => {
                log(
                    tx,
                    format!(
                        "dbsloc: note: precoreg: registration backend ants: the GPU registration \
                         failed: {e}; the run continues on ANTs"
                    ),
                );
                reg = RegChoice {
                    requested: reg.requested,
                    resolution: Resolution::Ants {
                        reason: Some(format!("the GPU registration failed: {e}")),
                    },
                    engine: None,
                };
                (
                    ants_rigid(find_ants, exec, template, moving, &tmp_prefix, opts, tx)?,
                    None,
                )
            }
            Some(Err(e)) => return Err(e),
            None => (
                ants_rigid(find_ants, exec, template, moving, &tmp_prefix, opts, tx)?,
                None,
            ),
        };

        let m = ras_map_moving_to_fixed(&total)?;
        let grid = fastwarp_reg::nifti_io::read_grid(moving)
            .map_err(|e| PipelineError::Message(format!("precoreg: {e}")))?;
        let new_affine = reheadered_affine(&grid.ras_affine(), &m);
        let tmp_image = staged.tmp(&image);
        write_reheadered(moving, &new_affine, &tmp_image)?;
        if let Err(e) = Volume::<f32>::load_nifti(&tmp_image) {
            return Err(PipelineError::Message(format!(
                "precoreg: written image does not load: {e}"
            )));
        }

        let (rotation_deg, angle_deg, translation_mm) = rigid_summary(&m);
        let mut rec = Map::new();
        rec.insert("backend".into(), json!(reg.resolution.as_str()));
        rec.insert(
            "template".into(),
            json!(crate::paths::absolute(template).to_string_lossy()),
        );
        rec.insert("template_space".into(), json!(space));
        rec.insert("matrix_ras".into(), matrix_json(&m));
        rec.insert("rotation_deg".into(), json!(rotation_deg));
        rec.insert("rotation_angle_deg".into(), json!(angle_deg));
        rec.insert("translation_mm".into(), json!(translation_mm));
        rec.insert(
            "moving".into(),
            json!(crate::paths::absolute(moving).to_string_lossy()),
        );
        rec.extend(reg_fields(&reg, timings));
        let text = serde_json::to_string_pretty(&Value::Object(rec))?;
        std::fs::write(staged.tmp(&record), text + "\n")?;
        Ok(())
    })();
    staged.finish(res, Some(&stage_lock(sub, PRECOREG_STAGE)))?;
    let stem = file_name_of(&image);
    let stem = stem.strip_suffix(".nii.gz").unwrap_or(&stem).to_owned();
    log(tx, format!("[precoreg] done: {}", file_name_of(&image)));
    Ok(StageOutcome::done(format!("ok:precoreg:{stem}")))
}

pub fn precoreg_approved_outputs_match(sub: &Subject, opts: &RunOptions) -> Result<(), String> {
    let asked = opts.backend_for(crate::stages::Stage::Precoreg);
    if asked == RegBackend::Auto {
        return Ok(());
    }
    let Some(rec) = std::fs::read_to_string(derivatives::precoreg_record(sub))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return Ok(());
    };
    let made_with = rec
        .get("reg_backend")
        .and_then(Value::as_str)
        .unwrap_or("ants");
    if made_with != asked.as_str() {
        return Err(format!(
            "the approved outputs were made with the {made_with} registration backend, this run \
             asks for {}",
            asked.as_str()
        ));
    }
    Ok(())
}

fn ants_rigid(
    find_ants: &dyn Fn() -> PipelineResult<PathBuf>,
    exec: &mut ToolExec<'_>,
    template: &Path,
    moving: &Path,
    tmp_prefix: &Path,
    opts: &RunOptions,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<AffineTransform> {
    let ants = find_ants()?;
    log(
        tx,
        format!(
            "[precoreg] antsRegistration rigid {} → {}",
            moving.display(),
            template.display()
        ),
    );
    let argv = build_ants_precoreg_argv(&ants, moving, template, tmp_prefix, &opts.ants_extra);
    exec(&argv)?;
    let written = derivatives::suffixed(tmp_prefix, derivatives::ANTS_AFFINE_SUFFIX);
    if !written.is_file() {
        return Err(PipelineError::Message(format!(
            "precoreg: antsRegistration wrote no {}",
            file_name_of(&written).trim_start_matches(crate::outputs::PARTIAL_PREFIX)
        )));
    }
    first_affine(&written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rot_z(deg: f64) -> [[f64; 3]; 3] {
        let (s, c) = deg.to_radians().sin_cos();
        [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
    }

    fn flip(p: [f64; 3]) -> [f64; 3] {
        [-p[0], -p[1], p[2]]
    }

    fn apply(m: &Mat4, p: [f64; 3]) -> [f64; 3] {
        let mut out = [0.0; 3];
        for (r, v) in out.iter_mut().enumerate() {
            *v = m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3];
        }
        out
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_precoreg_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample_nifti(path: &Path) -> Grid {
        let grid = Grid::new(
            [5, 6, 7],
            [1.0, 1.2, 2.0],
            [-3.0, 4.0, 5.0],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .unwrap();
        let data: Vec<f32> = (0..grid.num_voxels())
            .map(|i| (i % 97) as f32 + 0.5)
            .collect();
        Volume::from_vec(grid, data)
            .unwrap()
            .save_nifti(path)
            .unwrap();
        grid
    }

    #[test]
    fn the_ras_map_sends_moving_points_to_their_fixed_counterparts() {
        let t = AffineTransform::new(rot_z(30.0), [3.0, -2.0, 5.0], [10.0, 5.0, -4.0]);
        let m = ras_map_moving_to_fixed(&t).unwrap();
        for p_fixed in [[0.0, 0.0, 0.0], [12.0, -7.5, 3.0], [-40.0, 22.0, 61.0]] {
            let p_moving = t.apply(p_fixed);
            let got = apply(&m, flip(p_moving));
            let want = flip(p_fixed);
            for a in 0..3 {
                assert!((got[a] - want[a]).abs() < 1e-9, "{got:?} vs {want:?}");
            }
        }
        assert_eq!(m[3], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_pure_translation_has_the_expected_ras_translation_and_no_rotation() {
        let t = AffineTransform::from_translation([3.0, -2.0, 5.0]);
        let m = ras_map_moving_to_fixed(&t).unwrap();
        let (rot, angle, trans) = rigid_summary(&m);
        assert!(rot.iter().all(|r| r.abs() < 1e-9) && angle.abs() < 1e-6);
        for (got, want) in trans.iter().zip([3.0, -2.0, -5.0]) {
            assert!((got - want).abs() < 1e-9, "{trans:?}");
        }
    }

    #[test]
    fn the_rigid_summary_recovers_a_rotation_about_z() {
        let t = AffineTransform::new(rot_z(20.0), [0.0; 3], [0.0; 3]);
        let m = ras_map_moving_to_fixed(&t).unwrap();
        let (rot, angle, _) = rigid_summary(&m);
        assert!((angle - 20.0).abs() < 1e-9, "{angle}");
        assert!((rot[2].abs() - 20.0).abs() < 1e-9 && rot[0].abs() < 1e-9 && rot[1].abs() < 1e-9);
    }

    #[test]
    fn the_header_rewrite_keeps_every_voxel_byte_and_makes_a_new_orthonormal_frame() {
        let d = scratch("rewrite");
        let src = d.join("in.nii");
        let old = sample_nifti(&src);
        let raw = std::fs::read(&src).unwrap();
        let t = AffineTransform::new(rot_z(25.0), [4.0, -3.0, 6.0], [1.0, 2.0, 3.0]);
        let m = ras_map_moving_to_fixed(&t).unwrap();
        let a = old.ras_affine();
        let a_new = reheadered_affine(&a, &m);
        let patched = rewrite_nifti_affine(&raw, &a_new).unwrap();
        assert_eq!(patched.len(), raw.len());
        assert_eq!(patched[352..], raw[352..]);
        let touched = |i: usize| {
            (OFF_PIXDIM..OFF_PIXDIM + 4).contains(&i)
                || (OFF_QFORM_CODE..OFF_SROW + 48).contains(&i)
        };
        for i in 0..352 {
            if !touched(i) {
                assert_eq!(patched[i], raw[i], "header byte {i}");
            }
        }
        let out = d.join("out.nii");
        std::fs::write(&out, &patched).unwrap();
        let grid = fastwarp_reg::nifti_io::read_grid(&out).unwrap();
        let got = grid.ras_affine();
        for r in 0..3 {
            for c in 0..4 {
                assert!((got[r][c] - a_new[r][c]).abs() < 1e-4, "{r}{c}: {got:?}");
            }
        }
        let sp = old.spacing();
        let unit = |m: &Mat4| {
            let mut u = [[0.0; 3]; 3];
            for r in 0..3 {
                for c in 0..3 {
                    u[r][c] = m[r][c] / sp[c];
                }
            }
            u
        };
        let (u_new, u_old) = (unit(&got), unit(&a));
        for i in 0..3 {
            for j in 0..3 {
                let ortho: f64 = (0..3).map(|k| u_new[i][k] * u_new[j][k]).sum();
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((ortho - want).abs() < 1e-6, "{ortho}");
            }
        }
        let mut rel = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    rel[i][j] += u_new[i][k] * u_old[j][k];
                }
            }
        }
        for i in 0..3 {
            for j in 0..3 {
                let dot: f64 = (0..3).map(|k| rel[i][k] * rel[j][k]).sum();
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((dot - want).abs() < 1e-6, "rotation of A'*A^-1: {dot}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn zero_form_codes_become_scanner_and_other_codes_are_kept() {
        let d = scratch("codes");
        let src = d.join("in.nii");
        let old = sample_nifti(&src);
        let mut raw = std::fs::read(&src).unwrap();
        raw[OFF_QFORM_CODE..OFF_QFORM_CODE + 2].copy_from_slice(&0u16.to_le_bytes());
        raw[OFF_SFORM_CODE..OFF_SFORM_CODE + 2].copy_from_slice(&4u16.to_le_bytes());
        let patched = rewrite_nifti_affine(&raw, &old.ras_affine()).unwrap();
        assert_eq!(get_u16(&patched, OFF_QFORM_CODE, true), 1);
        assert_eq!(get_u16(&patched, OFF_SFORM_CODE, true), 4);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_header_rewrite_refuses_foreign_files_and_non_millimetre_units() {
        let d = scratch("refuse");
        let src = d.join("in.nii");
        let old = sample_nifti(&src);
        let raw = std::fs::read(&src).unwrap();
        assert!(rewrite_nifti_affine(&raw[..100], &old.ras_affine()).is_err());
        let mut bad = raw.clone();
        bad[0..4].copy_from_slice(&540i32.to_le_bytes());
        assert!(rewrite_nifti_affine(&bad, &old.ras_affine())
            .unwrap_err()
            .contains("not a NIfTI-1"));
        let mut metres = raw.clone();
        metres[OFF_XYZT_UNITS] = 1;
        assert!(rewrite_nifti_affine(&metres, &old.ras_affine())
            .unwrap_err()
            .contains("unit"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_recipe_and_the_ants_argv_describe_the_same_stage() {
        let recipe = precoreg_recipe();
        let argv = build_ants_precoreg_argv(
            Path::new("antsRegistration"),
            Path::new("/m.nii.gz"),
            Path::new("/f.nii.gz"),
            Path::new("/o/pfx"),
            &[],
        );
        let args = recipe.to_ants_args("/f.nii.gz", "/m.nii.gz");
        let at = argv.iter().position(|a| a == "--transform").unwrap();
        assert_eq!(argv[at..at + args.len()], args[..]);
    }
    #[test]
    fn a_cancel_during_the_native_rigid_is_the_cancelled_outcome() {
        use fastwarp_reg::{Grid, Volume};
        let d = std::env::temp_dir().join(format!("dbsloc_precoreg_cancel_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let g = Grid::new(
            [12, 12, 12],
            [2.0; 3],
            [0.0; 3],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .unwrap();
        let blob: Vec<f32> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let r2 = [i, j, k]
                    .iter()
                    .map(|v| (*v as f32 - 5.5).powi(2))
                    .sum::<f32>();
                (-r2 / 18.0).exp()
            })
            .collect();
        let image = d.join("img.nii.gz");
        Volume::from_vec(g, blob)
            .unwrap()
            .save_nifti(&image)
            .unwrap();
        let set = AtomicBool::new(true);
        let e = native_rigid(
            &crate::reg_backend::CpuReferenceEngine,
            &image,
            &image,
            &d.join("pre"),
            None,
            Some(&set),
            None,
        )
        .unwrap_err();
        assert!(matches!(e, PipelineError::Cancelled), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
