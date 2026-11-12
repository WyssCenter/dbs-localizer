// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Isotropic resampling of a NIfTI onto a coarser grid (`specs/04`, `RunOptions::norm_isotropic_mm`).
//!
//! `antsRegistration` on a 0.5 mm template is the pipeline's memory ceiling — the observed
//! failure mode is exit `-9` (SIGKILL, OOM).
//!
//! Only the *registration* pair is coarsened. The resulting transform is still applied to the
//! post-op CT on the original template grid, because a coarse output grid blurs the electrode
//! artifact below what reconstruction can use (roadmap 1.5).

use std::path::{Path, PathBuf};

use fastwarp_niftiio::WriterOptions;
use ndarray::Array3;
use nifti::{IntoNdArray, NiftiObject, NiftiVolume, ReaderOptions};

use fastwarp_core::nifti_affine::header_affine_with_source;
use fastwarp_core::{voxel_spacing_rowmajor, Mat4, XformSource};

pub fn resampled_grid(
    shape: [usize; 3],
    spacing: [f64; 3],
    target_mm: f64,
) -> ([usize; 3], [Vec<f64>; 3]) {
    let mut out = [1usize; 3];
    let mut coords: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for a in 0..3 {
        let s = shape[a];
        let cur = spacing[a];
        let ratio = target_mm / cur;
        let t =
            ((((s.saturating_sub(1)) as f64) * cur / target_mm + 1e-9).floor() as usize + 1).max(1);
        let last = s.saturating_sub(1) as f64;
        coords[a] = (0..t).map(|i| (i as f64 * ratio).min(last)).collect();
        out[a] = t;
    }
    (out, coords)
}

fn sample_linear(data: &Array3<f64>, c: [f64; 3]) -> f64 {
    let (ni, nj, nk) = data.dim();
    let dims = [ni as i64, nj as i64, nk as i64];
    let lo = [c[0].floor(), c[1].floor(), c[2].floor()];
    let fr = [c[0] - lo[0], c[1] - lo[1], c[2] - lo[2]];
    let mut acc = 0.0;
    for corner in 0..8 {
        let mut idx = [0i64; 3];
        let mut w = 1.0;
        for a in 0..3 {
            let bit = (corner >> a) & 1;
            idx[a] = lo[a] as i64 + bit as i64;
            w *= if bit == 1 { fr[a] } else { 1.0 - fr[a] };
        }
        if w == 0.0 || (0..3).any(|a| idx[a] < 0 || idx[a] >= dims[a]) {
            continue;
        }
        acc += w * data[[idx[0] as usize, idx[1] as usize, idx[2] as usize]];
    }
    acc
}

/// Resample `src` onto an isotropic `mm` grid covering the same world extent (`specs/04` §5).
///
/// Returns `src` itself, writing nothing, when every axis already has `mm` spacing (within
/// 1e-6 mm). Otherwise the grid keeps the source's origin and direction cosines; along each
/// axis output voxel `i` samples source index `min(i · mm / spacing, n − 1)` trilinearly, and
/// the axis has `floor((n − 1) · spacing / mm + 1e-9) + 1` voxels, so the output never reaches
/// past the source's field of view. The written header's quaternion fields are cleared
/// (`qform_code = 0`); the `sform_code` keeps the space code of whichever form the source
/// affine came from (`specs/01` §2), or `1` for a pixdim-only source.
pub fn resample_isotropic(src: &Path, out: &Path, mm: f64) -> Result<PathBuf, String> {
    if !(mm.is_finite() && mm > 0.0) {
        return Err(format!("isotropic resample size must be > 0, got {mm}"));
    }
    let obj = ReaderOptions::new()
        .read_file(src)
        .map_err(|e| format!("read {}: {e}", src.display()))?;
    let hdr = obj.header().clone();
    let dim = obj.volume().dim().to_vec();
    if dim.len() < 3 {
        return Err(format!("{} is not 3-D (dim {dim:?})", src.display()));
    }
    if dim.iter().skip(3).any(|&d| d > 1) {
        return Err(format!(
            "{}: need 3D (or 4D with last dim 1) for normalize, got {dim:?}",
            src.display()
        ));
    }
    if dim.iter().take(3).any(|&d| d > 2048) {
        return Err(format!("{}: dims too large: {dim:?}", src.display()));
    }
    let (aff, source) = header_affine_with_source(&hdr);
    let spacing = voxel_spacing_rowmajor(&aff);
    if spacing.iter().all(|s| (s - mm).abs() < 1e-6) {
        return Ok(src.to_path_buf());
    }
    let sform_code = match source {
        XformSource::Sform => hdr.sform_code,
        XformSource::Qform => hdr.qform_code,
        XformSource::Pixdim => 1,
    };
    let data = obj
        .into_volume()
        .into_ndarray::<f64>()
        .map_err(|e| format!("volume {}: {e}", src.display()))?;
    let mut data = data;
    while data.ndim() > 3 {
        let last = ndarray::Axis(data.ndim() - 1);
        data = data.index_axis_move(last, 0);
    }
    let data = data
        .into_dimensionality::<ndarray::Ix3>()
        .map_err(|e| format!("volume {} is not 3-D: {e}", src.display()))?;
    let shape3 = [data.shape()[0], data.shape()[1], data.shape()[2]];
    let (out_n, coords) = resampled_grid(shape3, spacing, mm);

    let mut out_aff: Mat4 = [[0.0; 4]; 4];
    for a in 0..3 {
        let s = spacing[a].max(1e-12);
        for r in 0..3 {
            out_aff[r][a] = aff[r][a] / s * mm;
        }
    }
    for r in 0..3 {
        out_aff[r][3] = aff[r][3];
    }
    out_aff[3][3] = 1.0;

    let resampled = Array3::<f32>::from_shape_fn((out_n[0], out_n[1], out_n[2]), |(i, j, k)| {
        sample_linear(&data, [coords[0][i], coords[1][j], coords[2][k]]) as f32
    });

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let mut out_hdr = hdr;
    out_hdr.pixdim[1] = mm as f32;
    out_hdr.pixdim[2] = mm as f32;
    out_hdr.pixdim[3] = mm as f32;
    out_hdr.srow_x = [
        out_aff[0][0] as f32,
        out_aff[0][1] as f32,
        out_aff[0][2] as f32,
        out_aff[0][3] as f32,
    ];
    out_hdr.srow_y = [
        out_aff[1][0] as f32,
        out_aff[1][1] as f32,
        out_aff[1][2] as f32,
        out_aff[1][3] as f32,
    ];
    out_hdr.srow_z = [
        out_aff[2][0] as f32,
        out_aff[2][1] as f32,
        out_aff[2][2] as f32,
        out_aff[2][3] as f32,
    ];
    out_hdr.sform_code = sform_code;
    out_hdr.qform_code = 0;
    out_hdr.scl_slope = 1.0;
    out_hdr.scl_inter = 0.0;

    WriterOptions::new(out)
        .reference_header(&out_hdr)
        .write_nifti(&resampled)
        .map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(out.to_path_buf())
}

pub fn template_voxel_mm(template: &Path) -> Option<f64> {
    let grid = fastwarp_reg::nifti_io::read_grid(template).ok()?;
    let v = grid.spacing().into_iter().fold(f64::INFINITY, f64::min);
    (v.is_finite() && v > 0.0).then_some(v)
}

pub fn norm_grid_dims(template: &Path, iso_mm: Option<f64>) -> Option<[usize; 3]> {
    crate::stage_impl::finest_grid_of(template, iso_mm)
}

pub fn norm_grid_warning(opts: &crate::options::RunOptions) -> Option<String> {
    if !opts.stages.contains(&crate::stages::Stage::Normalize) {
        return None;
    }
    let mm = opts.norm_isotropic_mm.mm()?;
    let template = opts
        .normalization_template
        .as_ref()
        .filter(|t| t.is_file())?;
    let voxel = template_voxel_mm(template)?;
    let voxel = (voxel * 100.0).round() / 100.0;
    if mm >= voxel - 1e-6 {
        return None;
    }
    Some(format!(
        "normalize: registration grid {} mm is finer than the template's {} mm voxels; the template is only interpolated, which adds no detail but costs memory and time (choose {} mm or full)",
        crate::paths::py_float(mm),
        crate::paths::py_float(voxel),
        crate::paths::py_float(voxel),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastwarp_core::apply_rowmajor;
    use nifti::{NiftiHeader, RandomAccessNiftiVolume};

    fn affine_rowmajor(hdr: &NiftiHeader) -> Mat4 {
        fastwarp_core::nifti_affine::header_affine(hdr)
    }

    fn write_test_nifti(path: &Path, data: &Array3<f32>, spacing: f64) -> Mat4 {
        let (ni, nj, nk) = data.dim();
        let sp = spacing as f32;
        let hdr = NiftiHeader {
            dim: [3, ni as u16, nj as u16, nk as u16, 1, 1, 1, 1],
            pixdim: [1.0, sp, sp, sp, 0.0, 0.0, 0.0, 0.0],
            sform_code: 1,
            qform_code: 0,
            srow_x: [sp, 0.0, 0.0, -10.0],
            srow_y: [0.0, sp, 0.0, -20.0],
            srow_z: [0.0, 0.0, sp, -30.0],
            scl_slope: 1.0,
            scl_inter: 0.0,
            ..NiftiHeader::default()
        };
        WriterOptions::new(path)
            .reference_header(&hdr)
            .write_nifti(data)
            .expect("write fixture");
        affine_rowmajor(&hdr)
    }

    fn warn_opts(
        template: Option<PathBuf>,
        mm: crate::options::NormGrid,
        normalize: bool,
    ) -> crate::options::RunOptions {
        let mut o = crate::options::RunOptions::new("/nonexistent/bids", "01");
        o.stages = if normalize {
            vec![
                crate::stages::Stage::Preprocess,
                crate::stages::Stage::Normalize,
            ]
        } else {
            vec![crate::stages::Stage::Preprocess]
        };
        o.normalization_template = template;
        o.norm_isotropic_mm = mm;
        o
    }

    fn warn_template(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbsloc_warn_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.nii.gz");
        write_test_nifti(&p, &Array3::<f32>::zeros((8, 6, 4)), 1.0);
        p
    }

    #[test]
    fn template_voxel_and_grid_dims_from_header() {
        let t = warn_template("dims");
        assert_eq!(template_voxel_mm(&t), Some(1.0));
        assert_eq!(norm_grid_dims(&t, None), Some([8, 6, 4]));
        assert_eq!(norm_grid_dims(&t, Some(2.0)), Some([4, 3, 2]));
        let dir = t.parent().unwrap().to_path_buf();
        assert_eq!(template_voxel_mm(&t.with_file_name("missing.nii.gz")), None);
        assert_eq!(
            norm_grid_dims(&t.with_file_name("missing.nii.gz"), None),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn grid_warning_only_for_finer_than_voxel() {
        use crate::options::NormGrid;
        let t = warn_template("warn");
        let w = norm_grid_warning(&warn_opts(Some(t.clone()), NormGrid::Mm(0.5), true)).unwrap();
        assert_eq!(
            w,
            "normalize: registration grid 0.5 mm is finer than the template's 1.0 mm voxels; the template is only interpolated, which adds no detail but costs memory and time (choose 1.0 mm or full)"
        );
        assert!(norm_grid_warning(&warn_opts(Some(t.clone()), NormGrid::Mm(1.0), true)).is_none());
        assert!(norm_grid_warning(&warn_opts(Some(t.clone()), NormGrid::Mm(1.5), true)).is_none());
        assert!(norm_grid_warning(&warn_opts(Some(t.clone()), NormGrid::Full, true)).is_none());
        assert!(norm_grid_warning(&warn_opts(Some(t.clone()), NormGrid::Template, true)).is_none());
        assert!(norm_grid_warning(&warn_opts(Some(t.clone()), NormGrid::Mm(0.5), false)).is_none());
        let missing = t.with_file_name("missing.nii.gz");
        assert!(norm_grid_warning(&warn_opts(Some(missing), NormGrid::Mm(0.5), true)).is_none());
        assert!(norm_grid_warning(&warn_opts(None, NormGrid::Mm(0.5), true)).is_none());
        let dir = t.parent().unwrap().to_path_buf();
        let t2 = dir.join("t2.nii.gz");
        write_test_nifti(&t2, &Array3::<f32>::zeros((8, 6, 4)), 1.004);
        assert!(norm_grid_warning(&warn_opts(Some(t2), NormGrid::Mm(1.0), true)).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The coarse grid must describe the same physical box, or the registration is solved
    /// against a differently positioned image and the transform is wrong everywhere.
    #[test]
    fn resampled_grid_covers_the_same_world_extent() {
        let dir = std::env::temp_dir().join("dbsloc_resample_extent");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.nii.gz");
        let out = dir.join("out.nii.gz");
        let n = 21usize;
        let mut data = Array3::<f32>::zeros((n, n, n));
        for i in 0..n {
            for j in 0..n {
                for k in 0..n {
                    data[[i, j, k]] = (i + 2 * j + 3 * k) as f32;
                }
            }
        }
        let src_aff = write_test_nifti(&src, &data, 0.5);
        resample_isotropic(&src, &out, 1.0).expect("resample");

        let obj = ReaderOptions::new().read_file(&out).unwrap();
        let hdr = obj.header().clone();
        let dim = obj.volume().dim().to_vec();
        let out_aff = affine_rowmajor(&hdr);
        assert_eq!(
            voxel_spacing_rowmajor(&out_aff).map(|v| (v * 1e6).round()),
            [1e6; 3]
        );

        let src_far = apply_rowmajor(&src_aff, [(n - 1) as f64; 3]);
        let out_far = apply_rowmajor(
            &out_aff,
            [
                (dim[0] - 1) as f64,
                (dim[1] - 1) as f64,
                (dim[2] - 1) as f64,
            ],
        );
        for a in 0..3 {
            assert!(
                (src_far[a] - out_far[a]).abs() < 1.0,
                "axis {a}: source extends to {} but resampled to {}",
                src_far[a],
                out_far[a]
            );
        }
        assert!(
            apply_rowmajor(&out_aff, [0.0; 3])
                .iter()
                .zip(apply_rowmajor(&src_aff, [0.0; 3]).iter())
                .all(|(a, b)| (a - b).abs() < 1e-6),
            "origin must not move"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A linear ramp must survive resampling, since trilinear interpolation is exact on it.
    #[test]
    fn linear_ramp_is_reproduced_exactly() {
        let dir = std::env::temp_dir().join("dbsloc_resample_ramp");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.nii.gz");
        let out = dir.join("out.nii.gz");
        let n = 17usize;
        let mut data = Array3::<f32>::zeros((n, n, n));
        for i in 0..n {
            for j in 0..n {
                for k in 0..n {
                    data[[i, j, k]] = (2.0 * i as f64 + 3.0 * j as f64 + 5.0 * k as f64) as f32;
                }
            }
        }
        write_test_nifti(&src, &data, 1.0);
        resample_isotropic(&src, &out, 2.0).expect("resample");

        let obj = ReaderOptions::new().read_file(&out).unwrap();
        let dim = obj.volume().dim().to_vec();
        let vol = obj.into_volume();
        for (i, j, k) in [(0usize, 0usize, 0usize), (1, 2, 3), (4, 4, 4)] {
            if i >= dim[0] as usize || j >= dim[1] as usize || k >= dim[2] as usize {
                continue;
            }
            let got: f64 = vol.get_f64(&[i as u16, j as u16, k as u16]).unwrap();
            let want = 2.0 * (2 * i) as f64 + 3.0 * (2 * j) as f64 + 5.0 * (2 * k) as f64;
            assert!((got - want).abs() < 1e-3, "({i},{j},{k}): {got} != {want}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn qform_only_source_keeps_its_origin_and_rotation() {
        let dir = std::env::temp_dir().join("dbsloc_resample_qform");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.nii.gz");
        let out = dir.join("out.nii.gz");
        let n = 9usize;
        let data = Array3::<f32>::from_elem((n, n, n), 1.0);
        let hdr = NiftiHeader {
            dim: [3, n as u16, n as u16, n as u16, 1, 1, 1, 1],
            pixdim: [-1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            sform_code: 0,
            qform_code: 2,
            quatern_b: 0.0,
            quatern_c: 0.0,
            quatern_d: 1.0,
            quatern_x: 40.0,
            quatern_y: -50.0,
            quatern_z: 60.0,
            scl_slope: 1.0,
            scl_inter: 0.0,
            ..NiftiHeader::default()
        };
        WriterOptions::new(&src)
            .reference_header(&hdr)
            .write_nifti(&data)
            .expect("write fixture");
        resample_isotropic(&src, &out, 2.0).expect("resample");

        let obj = ReaderOptions::new().read_file(&out).unwrap();
        let out_hdr = obj.header().clone();
        assert_eq!(out_hdr.sform_code, 2);
        assert_eq!(out_hdr.qform_code, 0);
        let out_aff = affine_rowmajor(&out_hdr);
        let want: Mat4 = [
            [-2.0, 0.0, 0.0, 40.0],
            [0.0, -2.0, 0.0, -50.0],
            [0.0, 0.0, -2.0, 60.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        for r in 0..4 {
            for c in 0..4 {
                assert!(
                    (out_aff[r][c] - want[r][c]).abs() < 1e-5,
                    "{out_aff:?} != {want:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_like_grid_shape_matches_the_python_rule() {
        let shape = [394usize, 466, 378];
        let (n15, c15) = resampled_grid(shape, [0.5; 3], 1.5);
        assert_eq!(n15, [132, 156, 126]);
        for (c, s) in c15.iter().zip(shape) {
            for (i, v) in c.iter().enumerate() {
                assert!((v - 3.0 * i as f64).abs() < 1e-12);
            }
            assert!(*c.last().unwrap() <= (s - 1) as f64);
        }
        let (n10, _) = resampled_grid(shape, [0.5; 3], 1.0);
        assert_eq!(n10, [197, 233, 189]);
        let (n_aniso, _) = resampled_grid([29, 31, 9], [0.43, 0.43, 1.0], 1.0);
        assert_eq!(n_aniso, [13, 13, 9]);
    }

    #[test]
    fn oblique_linear_field_is_sampled_at_output_world_positions() {
        let dir =
            std::env::temp_dir().join(format!("dbsloc_resample_oblique_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.nii.gz");
        let out = dir.join("out.nii.gz");
        let (t1, t2, t3) = (0.21f64, -0.13f64, 0.37f64);
        let rx = [
            [1.0, 0.0, 0.0],
            [0.0, t1.cos(), -t1.sin()],
            [0.0, t1.sin(), t1.cos()],
        ];
        let ry = [
            [t2.cos(), 0.0, t2.sin()],
            [0.0, 1.0, 0.0],
            [-t2.sin(), 0.0, t2.cos()],
        ];
        let rz = [
            [t3.cos(), -t3.sin(), 0.0],
            [t3.sin(), t3.cos(), 0.0],
            [0.0, 0.0, 1.0],
        ];
        let mul = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| {
            let mut m = [[0.0; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
                }
            }
            m
        };
        let r = mul(mul(rz, ry), rx);
        let flip = [-1.0, 1.0, -1.0];
        let sp = 0.5f64;
        let mut srow = [[0.0f32; 4]; 3];
        for i in 0..3 {
            for j in 0..3 {
                srow[i][j] = (r[i][j] * flip[j] * sp) as f32;
            }
        }
        srow[0][3] = 41.5;
        srow[1][3] = -73.25;
        srow[2][3] = 12.0;
        let n = [31usize, 27, 23];
        let hdr = NiftiHeader {
            dim: [3, n[0] as u16, n[1] as u16, n[2] as u16, 1, 1, 1, 1],
            pixdim: [1.0, sp as f32, sp as f32, sp as f32, 0.0, 0.0, 0.0, 0.0],
            sform_code: 2,
            qform_code: 0,
            srow_x: srow[0],
            srow_y: srow[1],
            srow_z: srow[2],
            scl_slope: 1.0,
            scl_inter: 0.0,
            ..NiftiHeader::default()
        };
        let src_aff = affine_rowmajor(&hdr);
        let f = |w: [f64; 3]| 0.7 * w[0] - 1.3 * w[1] + 2.1 * w[2] + 5.0;
        let data = Array3::<f64>::from_shape_fn((n[0], n[1], n[2]), |(i, j, k)| {
            f(apply_rowmajor(&src_aff, [i as f64, j as f64, k as f64]))
        });
        WriterOptions::new(&src)
            .reference_header(&hdr)
            .write_nifti(&data)
            .unwrap();
        resample_isotropic(&src, &out, 1.5).unwrap();
        let obj = ReaderOptions::new().read_file(&out).unwrap();
        let out_aff = affine_rowmajor(obj.header());
        let dims = obj.volume().dim().to_vec();
        assert_eq!(&dims[..3], &[11, 9, 8]);
        let vol = obj.into_volume().into_ndarray::<f64>().unwrap();
        let mut worst = 0.0f64;
        for i in 0..dims[0] as usize {
            for j in 0..dims[1] as usize {
                for k in 0..dims[2] as usize {
                    let w = apply_rowmajor(&out_aff, [i as f64, j as f64, k as f64]);
                    worst = worst.max((vol[[i, j, k]] - f(w)).abs());
                }
            }
        }
        assert!(worst < 1e-3, "worst deviation {worst}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_spacing_returns_the_source_and_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("dbsloc_resample_noop_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.nii.gz");
        let out = dir.join("out.nii.gz");
        write_test_nifti(&src, &Array3::<f32>::zeros((5, 6, 7)), 1.0);
        assert_eq!(resample_isotropic(&src, &out, 1.0).unwrap(), src);
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_nonsense_size() {
        let p = Path::new("nonexistent.nii.gz");
        for mm in [0.0, -1.0, f64::NAN] {
            assert!(resample_isotropic(p, p, mm).is_err(), "{mm} accepted");
        }
    }
}
