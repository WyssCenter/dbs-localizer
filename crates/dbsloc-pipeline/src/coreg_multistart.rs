// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! CT → anchor multi-start rigid initialisation (`specs/03` §5.1). Pure helpers, no binaries.
//!
//! Multi-start coregistration; the process orchestration lives in
//! `stage_impl::run_coregister`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use fastwarp_niftiio::WriterOptions;
use ndarray::{Array3, Axis, Ix3};
use nifti::{IntoNdArray, NiftiHeader, NiftiObject, ReaderOptions};
use serde::Serialize;
use thiserror::Error;

use fastwarp_core::{apply_rowmajor, Mat4};

use crate::ants_argv::{
    COARSE_RIGID_CONVERGENCE, COARSE_RIGID_SHRINK_FACTORS, COARSE_RIGID_SMOOTHING_SIGMAS,
    COARSE_RIGID_TRANSFORM,
};
use crate::error::PipelineResult;
use crate::stage_impl::affine_4x4;

pub const PITCH_STARTS_DEG: [f64; 5] = [-40.0, -20.0, 0.0, 20.0, 40.0];
pub const FIXED_HEAD_P99_FRACTION: f64 = 0.15;
pub const CT_HEAD_MIN_HU: f64 = -300.0;
pub const FIXED_MASK_NAME: &str = "fixed_mask.nii.gz";
pub const FIXED_MASK_EROSION_VOXELS: usize = 2;
const MAX_DIM: u16 = 2048;

#[derive(Debug, Error)]
pub enum MultistartError {
    #[error("NIfTI read {path}: {message}")]
    NiftiRead { path: PathBuf, message: String },
    #[error("{path}: dims too large: {n0}×{n1}×{n2}")]
    NiftiDimsTooLarge {
        path: PathBuf,
        n0: u16,
        n1: u16,
        n2: u16,
    },
    #[error("{path}: expected a 3-D volume, got {ndim}-D")]
    NotThreeD { path: PathBuf, ndim: usize },
    #[error("head centroid: fixed image has no finite voxels")]
    NoFiniteVoxels,
    #[error("head centroid: no finite voxels above {threshold}")]
    EmptyHeadMask { threshold: f64 },
    #[error("every multi-start coarse rigid start failed")]
    AllStartsFailed,
    #[error("fixed mask: no finite non-zero voxels survive the erosion")]
    EmptyFixedMask,
    #[error("NIfTI write {path}: {message}")]
    NiftiWrite { path: PathBuf, message: String },
}

/// Scaled voxel data (first volume of a 4-D series) and the world RAS affine.
pub fn load_volume(path: &Path) -> Result<(Array3<f32>, Mat4), MultistartError> {
    load_volume_with_header(path).map(|(d, a, _)| (d, a))
}

/// [`load_volume`] plus the file's header, the reference for [`write_fixed_mask`].
pub fn load_volume_with_header(
    path: &Path,
) -> Result<(Array3<f32>, Mat4, NiftiHeader), MultistartError> {
    let read_err = |message: String| MultistartError::NiftiRead {
        path: path.to_path_buf(),
        message,
    };
    let hdr = NiftiHeader::from_file(path).map_err(|e| read_err(e.to_string()))?;
    let ndim = usize::from(hdr.dim[0]);
    if ndim < 3 {
        return Err(MultistartError::NotThreeD {
            path: path.to_path_buf(),
            ndim,
        });
    }
    let (n0, n1, n2) = (hdr.dim[1], hdr.dim[2], hdr.dim[3]);
    if n0 > MAX_DIM || n1 > MAX_DIM || n2 > MAX_DIM {
        return Err(MultistartError::NiftiDimsTooLarge {
            path: path.to_path_buf(),
            n0,
            n1,
            n2,
        });
    }
    let obj = ReaderOptions::new()
        .read_file(path)
        .map_err(|e| read_err(e.to_string()))?;
    let header = obj.header().clone();
    let aff = affine_4x4(&header);
    let mut data = obj
        .into_volume()
        .into_ndarray::<f32>()
        .map_err(|e| read_err(e.to_string()))?;
    while data.ndim() > 3 {
        let last = Axis(data.ndim() - 1);
        data = data.index_axis_move(last, 0);
    }
    let ndim = data.ndim();
    let data = data
        .into_dimensionality::<Ix3>()
        .map_err(|_| MultistartError::NotThreeD {
            path: path.to_path_buf(),
            ndim,
        })?;
    Ok((data, aff, header))
}

/// Keep a voxel only if every voxel within `±radius` along each grid axis is in `mask`.
///
/// Neighbours are taken from the input mask on each axis separately (a cross, not a box);
/// voxels outside the volume count as outside the mask.
pub fn erode_along_axes(mask: &Array3<bool>, radius: usize) -> Array3<bool> {
    let (n0, n1, n2) = mask.dim();
    let n = [n0, n1, n2];
    Array3::from_shape_fn(mask.dim(), |(i, j, k)| {
        if !mask[[i, j, k]] {
            return false;
        }
        let p = [i, j, k];
        for a in 0..3 {
            for d in 1..=radius {
                if p[a] < d || p[a] + d >= n[a] {
                    return false;
                }
                let mut lo = p;
                lo[a] -= d;
                let mut hi = p;
                hi[a] += d;
                if !mask[lo] || !mask[hi] {
                    return false;
                }
            }
        }
        true
    })
}

/// Finite, non-zero fixed voxels eroded by 2 along each axis (`specs/03` §5.1 step 4).
pub fn fixed_registration_mask(data: &Array3<f32>) -> Result<Array3<bool>, MultistartError> {
    let base = data.mapv(|v| v.is_finite() && v != 0.0);
    let mask = erode_along_axes(&base, FIXED_MASK_EROSION_VOXELS);
    if mask.iter().any(|&b| b) {
        Ok(mask)
    } else {
        Err(MultistartError::EmptyFixedMask)
    }
}

pub fn fixed_mask_path(work_dir: &Path) -> PathBuf {
    work_dir.join(FIXED_MASK_NAME)
}

/// `uint8` NIfTI of `mask` on the fixed image's grid, with the fixed header as reference.
pub fn write_fixed_mask(
    path: &Path,
    mask: &Array3<bool>,
    reference: &NiftiHeader,
) -> Result<(), MultistartError> {
    let data = mask.mapv(u8::from);
    WriterOptions::new(path)
        .reference_header(reference)
        .write_nifti(&data)
        .map_err(|e| MultistartError::NiftiWrite {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

/// Best coarse start's transform and the fixed-image mask for the full rigid run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultistartInit {
    pub transform: PathBuf,
    pub fixed_mask: PathBuf,
}

/// Linear-interpolated percentile with NumPy's default (`method="linear"`) arithmetic.
fn percentile_linear(values: &mut [f32], q_pct: f64) -> Option<f64> {
    let n = values.len();
    if n == 0 {
        return None;
    }
    let q = q_pct / 100.0;
    let vi = (n as f64 * q + (1.0 - q)) - 1.0;
    if vi >= (n - 1) as f64 {
        return values.iter().copied().reduce(f32::max).map(f64::from);
    }
    let lo_f = vi.floor().max(0.0);
    let lo = lo_f as usize;
    let gamma = vi - lo_f;
    let (_, a, right) = values.select_nth_unstable_by(lo, f32::total_cmp);
    let a = f64::from(*a);
    let b = f64::from(right.iter().copied().fold(f32::INFINITY, f32::min));
    let d = b - a;
    Some(if gamma >= 0.5 {
        b - d * (1.0 - gamma)
    } else {
        a + d * gamma
    })
}

/// `0.15 × p99` of the finite voxels of the fixed image.
pub fn fixed_head_threshold(data: &Array3<f32>) -> Result<f64, MultistartError> {
    let mut finite: Vec<f32> = data.iter().copied().filter(|v| v.is_finite()).collect();
    let p99 = percentile_linear(&mut finite, 99.0).ok_or(MultistartError::NoFiniteVoxels)?;
    Ok(FIXED_HEAD_P99_FRACTION * p99)
}

/// Mean world position (LPS mm) of the finite voxels strictly above `threshold`.
pub fn head_centroid_lps(
    data: &Array3<f32>,
    aff: &Mat4,
    threshold: f64,
) -> Result<[f64; 3], MultistartError> {
    let (mut n, mut si, mut sj, mut sk) = (0u64, 0u64, 0u64, 0u64);
    for ((i, j, k), &v) in data.indexed_iter() {
        let v = f64::from(v);
        if v.is_finite() && v > threshold {
            n += 1;
            si += i as u64;
            sj += j as u64;
            sk += k as u64;
        }
    }
    if n == 0 {
        return Err(MultistartError::EmptyHeadMask { threshold });
    }
    let nf = n as f64;
    let ras = apply_rowmajor(aff, [si as f64 / nf, sj as f64 / nf, sk as f64 / nf]);
    Ok([-ras[0], -ras[1], ras[2]])
}

pub fn fixed_head_centroid_lps(
    data: &Array3<f32>,
    aff: &Mat4,
) -> Result<[f64; 3], MultistartError> {
    head_centroid_lps(data, aff, fixed_head_threshold(data)?)
}

pub fn ct_head_centroid_lps(data: &Array3<f32>, aff: &Mat4) -> Result<[f64; 3], MultistartError> {
    head_centroid_lps(data, aff, CT_HEAD_MIN_HU)
}

/// `R_x(θ)`: right-handed rotation about the LPS *x* (left–right) axis.
pub fn pitch_rotation_lps(pitch_deg: f64) -> [[f64; 3]; 3] {
    let t = pitch_deg.to_radians();
    let (s, c) = t.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn fmt12(v: f64) -> String {
    format!("{:.12}", v + 0.0)
}

/// ITK text transform `T(p) = R·(p − c_f) + c_m` (centre `c_f`, translation `c_m − c_f`).
pub fn itk_affine_text(r: &[[f64; 3]; 3], c_f: [f64; 3], c_m: [f64; 3]) -> String {
    let params: Vec<String> = r
        .iter()
        .flatten()
        .copied()
        .chain((0..3).map(|a| c_m[a] - c_f[a]))
        .map(fmt12)
        .collect();
    let fixed: Vec<String> = c_f.iter().copied().map(fmt12).collect();
    format!(
        "#Insight Transform File V1.0\n#Transform 0\nTransform: AffineTransform_double_3_3\n\
         Parameters: {}\nFixedParameters: {}\n",
        params.join(" "),
        fixed.join(" ")
    )
}

pub fn write_itk_affine_text(
    path: &Path,
    r: &[[f64; 3]; 3],
    c_f: [f64; 3],
    c_m: [f64; 3],
) -> std::io::Result<()> {
    std::fs::write(path, itk_affine_text(r, c_f, c_m))
}

/// Metric of one `…DIAGNOSTIC, <iteration>, <metric>, …` data line; `None` otherwise.
pub fn diagnostic_metric(line: &str) -> Option<f64> {
    let (_, rest) = line.split_once("DIAGNOSTIC,")?;
    let mut fields = rest.split(',');
    let iteration = fields.next()?.trim();
    let metric = fields.next()?.trim();
    iteration.parse::<i64>().ok()?;
    metric.parse::<f64>().ok()
}

/// Last finite `DIAGNOSTIC` metric of `antsRegistration --verbose 1`, fed one stdout line at a time.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiagnosticTracker {
    last: Option<f64>,
}

impl DiagnosticTracker {
    pub fn push_line(&mut self, line: &str) {
        if let Some(v) = diagnostic_metric(line) {
            self.last = Some(v);
        }
    }

    pub fn finish(&self) -> Option<f64> {
        self.last.filter(|v| v.is_finite())
    }
}

/// Metric on the last `DIAGNOSTIC` iteration line of `antsRegistration --verbose 1`.
#[cfg(test)]
fn parse_last_diagnostic_metric(stdout: &str) -> Option<f64> {
    let mut t = DiagnosticTracker::default();
    for line in stdout.lines() {
        t.push_line(line);
    }
    t.finish()
}

pub fn start_score(metric: Option<f64>) -> f64 {
    match metric {
        Some(v) if v.is_finite() => v,
        _ => f64::INFINITY,
    }
}

/// Index of the lowest score; failed starts count as `+inf`; ties go to the first.
pub fn select_best_start(scores: &[Option<f64>]) -> Result<usize, MultistartError> {
    let mut best: Option<(usize, f64)> = None;
    for (i, &s) in scores.iter().enumerate() {
        let v = start_score(s);
        if v < best.map_or(f64::INFINITY, |b| b.1) {
            best = Some((i, v));
        }
    }
    best.map(|b| b.0).ok_or(MultistartError::AllStartsFailed)
}

pub fn start_tag(pitch_deg: f64) -> String {
    let sign = if pitch_deg < 0.0 { 'm' } else { 'p' };
    format!("{sign}{:02}", pitch_deg.abs().round() as i64)
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s: OsString = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Work files of one start under `coregistration/log/multistart/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPaths {
    pub start_txt: PathBuf,
    pub coarse_prefix: PathBuf,
    pub coarse_mat: PathBuf,
}

pub fn multistart_work_paths(work: &Path, pitch_deg: f64) -> StartPaths {
    let tag = start_tag(pitch_deg);
    let coarse_prefix = work.join(format!("coarse_{tag}_"));
    StartPaths {
        start_txt: work.join(format!("start_{tag}.txt")),
        coarse_mat: with_suffix(&coarse_prefix, "0GenericAffine.mat"),
        coarse_prefix,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartRecord {
    pub pitch_deg: f64,
    pub metric: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_backend: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoarseSchedule {
    pub transform: &'static str,
    pub metric: &'static str,
    pub convergence: &'static str,
    pub shrink_factors: &'static str,
    pub smoothing_sigmas: &'static str,
}

pub const COARSE_SCHEDULE: CoarseSchedule = CoarseSchedule {
    transform: COARSE_RIGID_TRANSFORM,
    metric: "MI[fixed,moving,1,32,Regular,0.25]",
    convergence: COARSE_RIGID_CONVERGENCE,
    shrink_factors: COARSE_RIGID_SHRINK_FACTORS,
    smoothing_sigmas: COARSE_RIGID_SMOOTHING_SIGMAS,
};

/// `sub-<id>_ses-postop_desc-multistart_CT.json` (`specs/03` §5.1 step 5).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultistartRecord {
    pub starts: Vec<StartRecord>,
    pub chosen_pitch_deg: f64,
    pub fixed_centroid_lps: [f64; 3],
    pub moving_centroid_lps: [f64; 3],
    pub coarse_schedule: CoarseSchedule,
    pub fixed_mask: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reg_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reg_requested: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reg_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reg_adapter: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reg_timings: Option<serde_json::Value>,
}

pub fn multistart_record(
    pitches_deg: &[f64],
    scores: &[Option<f64>],
    chosen: usize,
    c_f: [f64; 3],
    c_m: [f64; 3],
    fixed_mask: &Path,
    metric_backend: &str,
) -> MultistartRecord {
    MultistartRecord {
        starts: pitches_deg
            .iter()
            .zip(scores)
            .map(|(&pitch_deg, &s)| {
                let v = start_score(s);
                StartRecord {
                    pitch_deg,
                    metric: v.is_finite().then_some(v),
                    metric_backend: Some(metric_backend.to_owned()),
                }
            })
            .collect(),
        chosen_pitch_deg: pitches_deg.get(chosen).copied().unwrap_or(f64::NAN),
        fixed_centroid_lps: c_f,
        moving_centroid_lps: c_m,
        coarse_schedule: COARSE_SCHEDULE,
        fixed_mask: fixed_mask.to_string_lossy().into_owned(),
        reg_backend: None,
        reg_requested: None,
        reg_note: None,
        reg_adapter: None,
        reg_timings: None,
    }
}

pub fn write_multistart_record(path: &Path, record: &MultistartRecord) -> PipelineResult<()> {
    let mut s = serde_json::to_string_pretty(record)?;
    s.push('\n');
    std::fs::write(path, s)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use fastwarp_niftiio::WriterOptions;

    const AFF: Mat4 = [
        [2.0, 0.0, 0.0, -10.0],
        [0.0, 2.0, 0.0, -20.0],
        [0.0, 0.0, 2.0, -30.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    const EYE: Mat4 = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    const ANTS_SAMPLE: &str = "All_Command_lines_OK
DIAGNOSTIC,Iteration,metricValue,convergenceValue,ITERATION_TIME_INDEX,SINCE_LAST
 1DIAGNOSTIC,     1, -2.100000000000e-01, inf, 1.2000e-01, 1.2000e-01,
DIAGNOSTIC,Iteration,metricValue,convergenceValue,ITERATION_TIME_INDEX,SINCE_LAST
 2DIAGNOSTIC,     9, -3.811000000000e-01, 6.100000000000e-03, 3.5000e+00, 5.0000e-02,
 2DIAGNOSTIC,    10, -3.838280657510e-01, 5.300144164617e-03, 3.5628e+00, 5.3787e-02,
  Elapsed time (stage 0): 3.6
";

    fn cube(n: usize, lo: usize, hi: usize, value: f32) -> Array3<f32> {
        let mut d = Array3::<f32>::zeros((n, n, n));
        for i in lo..hi {
            for j in lo..hi {
                for k in lo..hi {
                    d[[i, j, k]] = value;
                }
            }
        }
        d
    }

    fn assert_close3(a: [f64; 3], b: [f64; 3]) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-9, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn fixed_centroid_is_cube_centre_in_lps() {
        let d = cube(12, 2, 8, 100.0);
        let c = fixed_head_centroid_lps(&d, &AFF).unwrap();
        let ras = apply_rowmajor(&AFF, [4.5, 4.5, 4.5]);
        assert_close3(c, [-ras[0], -ras[1], ras[2]]);
    }

    #[test]
    fn percentile_matches_numpy_linear() {
        let mut v: Vec<f32> = (0..1000u16).map(f32::from).collect();
        assert!((percentile_linear(&mut v, 99.0).unwrap() - 989.01).abs() < 1e-9);
        let mut w = vec![3.0f32, 1.0, 2.0, 10.0];
        assert!((percentile_linear(&mut w, 99.0).unwrap() - 9.79).abs() < 1e-9);
        assert_eq!(percentile_linear(&mut [7.0f32], 99.0), Some(7.0));
        assert_eq!(percentile_linear(&mut [] as &mut [f32], 99.0), None);
    }

    #[test]
    fn centroid_ignores_nan_and_inf() {
        let mut d = Array3::<f32>::from_elem((10, 10, 10), -1000.0);
        for i in 1..5 {
            for j in 1..5 {
                for k in 1..5 {
                    d[[i, j, k]] = 40.0;
                }
            }
        }
        d[[9, 9, 9]] = f32::NAN;
        d[[0, 9, 9]] = f32::INFINITY;
        d[[9, 0, 9]] = f32::NEG_INFINITY;
        assert_close3(ct_head_centroid_lps(&d, &EYE).unwrap(), [-2.5, -2.5, 2.5]);
        let f = d.mapv(|v| if v > 0.0 { v } else { 0.0 });
        assert_close3(
            fixed_head_centroid_lps(&f, &EYE).unwrap(),
            [-2.5, -2.5, 2.5],
        );
    }

    #[test]
    fn empty_masks_are_errors() {
        let d = Array3::<f32>::from_elem((6, 6, 6), -1000.0);
        assert!(matches!(
            ct_head_centroid_lps(&d, &EYE),
            Err(MultistartError::EmptyHeadMask { .. })
        ));
        let nan = Array3::<f32>::from_elem((4, 4, 4), f32::NAN);
        assert!(matches!(
            fixed_head_threshold(&nan),
            Err(MultistartError::NoFiniteVoxels)
        ));
    }

    #[test]
    fn itk_text_exact_format() {
        let eye = pitch_rotation_lps(0.0);
        let text = itk_affine_text(&eye, [1.0, 2.0, 3.0], [4.0, 6.0, 8.5]);
        assert_eq!(
            text,
            "#Insight Transform File V1.0\n#Transform 0\nTransform: AffineTransform_double_3_3\n\
             Parameters: 1.000000000000 0.000000000000 0.000000000000 0.000000000000 \
             1.000000000000 0.000000000000 0.000000000000 0.000000000000 1.000000000000 \
             3.000000000000 4.000000000000 5.500000000000\n\
             FixedParameters: 1.000000000000 2.000000000000 3.000000000000\n"
        );
    }

    #[test]
    fn itk_text_maps_fixed_centroid_to_moving_centroid() {
        let c_f = [-12.5, 30.25, 41.0];
        let c_m = [3.0, -7.5, 60.125];
        let rot = pitch_rotation_lps(-40.0);
        let text = itk_affine_text(&rot, c_f, c_m);
        let nums = |prefix: &str| -> Vec<f64> {
            let line = text.lines().find(|l| l.starts_with(prefix)).unwrap();
            line[prefix.len()..]
                .split_whitespace()
                .map(|s| s.parse().unwrap())
                .collect()
        };
        let p = nums("Parameters:");
        let c = nums("FixedParameters:");
        let apply = |x: [f64; 3]| -> [f64; 3] {
            let mut out = [0.0; 3];
            for r in 0..3 {
                out[r] =
                    (0..3).map(|k| p[3 * r + k] * (x[k] - c[k])).sum::<f64>() + c[r] + p[9 + r];
            }
            out
        };
        assert_close3(apply(c_f), c_m);
        let moved = apply([c_f[0], c_f[1] + 10.0, c_f[2]]);
        assert_close3(
            moved,
            [c_m[0], c_m[1] + 10.0 * rot[1][1], c_m[2] + 10.0 * rot[2][1]],
        );
        assert!(!itk_affine_text(&[[-0.0; 3]; 3], c_f, c_f).contains("-0.000000000000"));
    }

    #[test]
    fn rotation_is_about_lps_x() {
        let r = pitch_rotation_lps(20.0);
        let t = 20f64.to_radians();
        assert_eq!(r[0], [1.0, 0.0, 0.0]);
        assert!((r[1][1] - t.cos()).abs() < 1e-15 && (r[1][2] + t.sin()).abs() < 1e-15);
        assert!((r[2][1] - t.sin()).abs() < 1e-15 && (r[2][2] - t.cos()).abs() < 1e-15);
    }

    #[test]
    fn parses_last_metric_and_skips_header() {
        let m = parse_last_diagnostic_metric(ANTS_SAMPLE).unwrap();
        assert!((m - -3.838280657510e-01).abs() < 1e-15);
        assert_eq!(
            parse_last_diagnostic_metric("DIAGNOSTIC,Iteration,metricValue,convergenceValue\n"),
            None
        );
        assert_eq!(parse_last_diagnostic_metric(""), None);
        assert_eq!(
            parse_last_diagnostic_metric(" 1DIAGNOSTIC,     3, nan, inf,\n"),
            None
        );
    }

    #[test]
    fn selection_rules() {
        assert_eq!(
            select_best_start(&[
                Some(-0.364),
                Some(-0.348),
                Some(-0.383),
                Some(-0.544),
                Some(-0.543)
            ])
            .unwrap(),
            3
        );
        assert_eq!(
            select_best_start(&[
                None,
                Some(-0.1),
                Some(f64::INFINITY),
                Some(-0.2),
                Some(f64::NAN)
            ])
            .unwrap(),
            3
        );
        assert_eq!(
            select_best_start(&[Some(-0.3), Some(-0.544), Some(-0.2), Some(-0.544)]).unwrap(),
            1
        );
        assert!(matches!(
            select_best_start(&[None, Some(f64::INFINITY), Some(f64::NAN)]),
            Err(MultistartError::AllStartsFailed)
        ));
    }

    #[test]
    fn work_paths_and_tags() {
        let tags: Vec<String> = PITCH_STARTS_DEG.iter().map(|&p| start_tag(p)).collect();
        assert_eq!(tags, ["m40", "m20", "p00", "p20", "p40"]);
        let work = Path::new("/d/coreg").join("multistart");
        let p = multistart_work_paths(&work, 20.0);
        assert_eq!(p.start_txt, work.join("start_p20.txt"));
        assert_eq!(p.coarse_prefix, work.join("coarse_p20_"));
        assert_eq!(p.coarse_mat, work.join("coarse_p20_0GenericAffine.mat"));
    }

    #[test]
    fn record_writes_null_for_failed_starts() {
        let scores = [
            Some(-0.364),
            None,
            Some(-0.383),
            Some(-0.544),
            Some(f64::INFINITY),
        ];
        let rec = multistart_record(
            &PITCH_STARTS_DEG,
            &scores,
            3,
            [1.0, 2.0, 3.0],
            [4.0, 5.0, 6.0],
            Path::new("/w/multistart/fixed_mask.nii.gz"),
            "ants",
        );
        let v = serde_json::to_value(&rec).unwrap();
        assert_eq!(v["fixed_mask"], "/w/multistart/fixed_mask.nii.gz");
        let text = serde_json::to_string(&rec).unwrap();
        assert!(text.find("\"coarse_schedule\"") < text.find("\"fixed_mask\""));
        assert_eq!(v["starts"][1]["metric"], serde_json::Value::Null);
        assert_eq!(v["starts"][4]["metric"], serde_json::Value::Null);
        assert_eq!(v["starts"][3]["metric"], -0.544);
        assert_eq!(v["starts"][3]["metric_backend"], "ants");
        assert_eq!(v["chosen_pitch_deg"], 20.0);
        assert_eq!(v["fixed_centroid_lps"], serde_json::json!([1.0, 2.0, 3.0]));
        assert_eq!(v["coarse_schedule"]["convergence"], "[200x100,1e-6,10]");
        assert_eq!(v["coarse_schedule"]["shrink_factors"], "8x4");
        assert_eq!(v["coarse_schedule"]["smoothing_sigmas"], "3x2vox");
    }

    #[test]
    fn load_volume_reads_affine_and_data() {
        let dir = std::env::temp_dir().join("dbsloc_coreg_multistart_load");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ct.nii.gz");
        let mut data = Array3::<f32>::from_elem((14, 14, 14), -1000.0);
        for i in 3..9 {
            for j in 4..10 {
                for k in 5..11 {
                    data[[i, j, k]] = 40.0;
                }
            }
        }
        data[[0, 0, 0]] = f32::NAN;
        let hdr = NiftiHeader {
            dim: [3, 14, 14, 14, 1, 1, 1, 1],
            pixdim: [1.0, 2.0, 2.0, 2.0, 0.0, 0.0, 0.0, 0.0],
            sform_code: 1,
            qform_code: 0,
            srow_x: [2.0, 0.0, 0.0, -10.0],
            srow_y: [0.0, 2.0, 0.0, -20.0],
            srow_z: [0.0, 0.0, 2.0, -30.0],
            scl_slope: 1.0,
            scl_inter: 0.0,
            ..NiftiHeader::default()
        };
        WriterOptions::new(&path)
            .reference_header(&hdr)
            .write_nifti(&data)
            .unwrap();
        let (d, aff) = load_volume(&path).unwrap();
        assert_eq!(d.dim(), (14, 14, 14));
        assert_eq!(aff, AFF);
        let ras = apply_rowmajor(&AFF, [5.5, 6.5, 7.5]);
        assert_close3(
            ct_head_centroid_lps(&d, &aff).unwrap(),
            [-ras[0], -ras[1], ras[2]],
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn naive_cross_erosion(m: &Array3<bool>, r: usize) -> Array3<bool> {
        let (n0, n1, n2) = m.dim();
        let n = [n0 as i64, n1 as i64, n2 as i64];
        let r = r as i64;
        Array3::from_shape_fn(m.dim(), |(i, j, k)| {
            let p = [i as i64, j as i64, k as i64];
            (0..3).all(|a| {
                (-r..=r).all(|d| {
                    let mut q = p;
                    q[a] += d;
                    q[a] >= 0 && q[a] < n[a] && m[[q[0] as usize, q[1] as usize, q[2] as usize]]
                })
            })
        })
    }

    #[test]
    fn erosion_is_a_cross_per_axis_not_a_box() {
        let mut m = Array3::from_elem((9, 9, 9), true);
        m[[2, 2, 2]] = false;
        let e = erode_along_axes(&m, 2);
        assert!(!e[[4, 2, 2]] && !e[[2, 4, 2]] && !e[[2, 2, 4]]);
        assert!(e[[3, 3, 2]], "diagonal neighbour of the hole survives");
        assert!(e[[4, 4, 4]]);
    }

    #[test]
    fn erosion_treats_outside_as_outside_without_wrap() {
        let m = Array3::from_elem((9, 9, 9), true);
        let e = erode_along_axes(&m, 2);
        let expected = Array3::from_shape_fn((9, 9, 9), |(i, j, k)| {
            (2..7).contains(&i) && (2..7).contains(&j) && (2..7).contains(&k)
        });
        assert_eq!(e, expected);
        let mut m2 = Array3::from_elem((9, 9, 9), false);
        for i in (0..3).chain(7..9) {
            for j in 0..9 {
                for k in 0..9 {
                    m2[[i, j, k]] = true;
                }
            }
        }
        let e2 = erode_along_axes(&m2, 2);
        assert!(!e2.iter().any(|&b| b));
        assert!(!erode_along_axes(&Array3::from_elem((3, 9, 9), true), 2)
            .iter()
            .any(|&b| b));
    }

    #[test]
    fn erosion_matches_naive_reference() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let m = Array3::from_shape_fn((11, 10, 9), |_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % 100 >= 15
        });
        assert_eq!(erode_along_axes(&m, 2), naive_cross_erosion(&m, 2));
    }

    #[test]
    fn fixed_mask_excludes_nan_inf_and_zero() {
        let mut d = Array3::<f32>::from_elem((9, 9, 9), 5.0);
        d[[4, 4, 1]] = f32::NAN;
        d[[1, 4, 4]] = 0.0;
        d[[4, 1, 4]] = f32::INFINITY;
        d[[6, 6, 6]] = -0.0;
        let m = fixed_registration_mask(&d).unwrap();
        assert!(!m[[4, 4, 3]] && !m[[3, 4, 4]] && !m[[4, 3, 4]] && !m[[6, 6, 4]]);
        assert!(m[[2, 2, 2]]);
        let neg = Array3::<f32>::from_elem((9, 9, 9), -3.0);
        assert!(fixed_registration_mask(&neg).unwrap()[[4, 4, 4]]);
    }

    #[test]
    fn fixed_mask_empty_is_an_error() {
        let zeros = Array3::<f32>::zeros((9, 9, 9));
        assert!(matches!(
            fixed_registration_mask(&zeros),
            Err(MultistartError::EmptyFixedMask)
        ));
        let thin = Array3::<f32>::from_elem((4, 9, 9), 1.0);
        assert!(matches!(
            fixed_registration_mask(&thin),
            Err(MultistartError::EmptyFixedMask)
        ));
    }

    #[test]
    fn fixed_mask_is_written_as_uint8_on_the_fixed_grid() {
        let dir = std::env::temp_dir().join("dbsloc_coreg_multistart_mask");
        std::fs::create_dir_all(&dir).unwrap();
        let fixed = dir.join("fixed.nii.gz");
        let hdr = NiftiHeader {
            dim: [3, 12, 12, 12, 1, 1, 1, 1],
            pixdim: [1.0, 2.0, 2.0, 2.0, 0.0, 0.0, 0.0, 0.0],
            sform_code: 1,
            qform_code: 0,
            srow_x: [2.0, 0.0, 0.0, -10.0],
            srow_y: [0.0, 2.0, 0.0, -20.0],
            srow_z: [0.0, 0.0, 2.0, -30.0],
            scl_slope: 1.0,
            scl_inter: 0.0,
            ..NiftiHeader::default()
        };
        WriterOptions::new(&fixed)
            .reference_header(&hdr)
            .write_nifti(&cube(12, 2, 8, 300.0))
            .unwrap();
        let (d, _, header) = load_volume_with_header(&fixed).unwrap();
        let m = fixed_registration_mask(&d).unwrap();
        let out = dir.join("fixed_mask.nii.gz");
        write_fixed_mask(&out, &m, &header).unwrap();

        let obj = ReaderOptions::new().read_file(&out).unwrap();
        assert_eq!(obj.header().datatype, nifti::NiftiType::Uint8 as i16);
        assert_eq!(affine_4x4(obj.header()), AFF);
        let back = obj.into_volume().into_ndarray::<u8>().unwrap();
        let back = back.into_dimensionality::<Ix3>().unwrap();
        let expected = Array3::from_shape_fn((12, 12, 12), |(i, j, k)| {
            u8::from((4..6).contains(&i) && (4..6).contains(&j) && (4..6).contains(&k))
        });
        assert_eq!(back, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
