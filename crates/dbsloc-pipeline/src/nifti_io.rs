// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::ops::Range;
use std::path::{Path, PathBuf};

use fastwarp_niftiio::{decode_map_c_order, NiftiFile, WriterOptions};
use ndarray::{s, Array3, Array4, ArrayD, Axis, Ix3, Ix4, IxDyn};
use nifti::{NiftiHeader, NiftiType};
use thiserror::Error;

use fastwarp_core::Mat4;

pub const MAX_DIM: usize = 2048;
const MAX_VOXELS: usize = MAX_DIM * MAX_DIM * MAX_DIM;

#[derive(Debug, Error)]
pub enum NiftiIoError {
    #[error("NIfTI read {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("NIfTI {path}: dims too large: {dims:?}")]
    DimsTooLarge { path: PathBuf, dims: Vec<usize> },
    #[error("NIfTI {path}: zero-sized dimension in {dims:?}")]
    EmptyDimension { path: PathBuf, dims: Vec<usize> },
    #[error("NIfTI {path}: expected a 3-D volume, got {ndim} dimension(s)")]
    NotThreeD { path: PathBuf, ndim: usize },
    #[error("NIfTI {path}: valid slope but invalid intercept {inter}")]
    InvalidIntercept { path: PathBuf, inter: f32 },
    #[error("NIfTI write {path}: {message}")]
    Write { path: PathBuf, message: String },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NiftiGrid {
    pub dims: [usize; 3],
    pub affine: Mat4,
}

fn read_err(path: &Path, e: impl std::fmt::Display) -> NiftiIoError {
    NiftiIoError::Read {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

fn header_dims(path: &Path, hdr: &NiftiHeader) -> Result<Vec<usize>, NiftiIoError> {
    let ndim = usize::from(hdr.dim[0]).clamp(1, 7);
    let dims: Vec<usize> = (1..=ndim).map(|i| usize::from(hdr.dim[i])).collect();
    let total = dims
        .iter()
        .try_fold(1usize, |acc, &d| acc.checked_mul(d.max(1)));
    if dims.iter().take(3).any(|&d| d > MAX_DIM) || total.is_none_or(|n| n > MAX_VOXELS) {
        return Err(NiftiIoError::DimsTooLarge {
            path: path.to_path_buf(),
            dims,
        });
    }
    if dims.contains(&0) {
        return Err(NiftiIoError::EmptyDimension {
            path: path.to_path_buf(),
            dims,
        });
    }
    Ok(dims)
}

fn to_standard<D: ndarray::Dimension, T: Clone>(a: ndarray::Array<T, D>) -> ndarray::Array<T, D> {
    if a.is_standard_layout() {
        a
    } else {
        a.as_standard_layout().into_owned()
    }
}

fn scaling(path: &Path, hdr: &NiftiHeader) -> Result<Option<(f64, f64)>, NiftiIoError> {
    let slope = hdr.scl_slope;
    if slope == 0.0 || !slope.is_finite() {
        return Ok(None);
    }
    let inter = hdr.scl_inter;
    if !inter.is_finite() {
        return Err(NiftiIoError::InvalidIntercept {
            path: path.to_path_buf(),
            inter,
        });
    }
    if slope == 1.0 && inter == 0.0 {
        return Ok(None);
    }
    Ok(Some((f64::from(slope), f64::from(inter))))
}

struct RawVolume {
    bytes: Vec<u8>,
    datatype: NiftiType,
    big_endian: bool,
    dims: Vec<usize>,
}

fn volume_byte_len(path: &Path, dims: &[usize], size: usize) -> Result<usize, NiftiIoError> {
    dims.iter()
        .try_fold(size, |acc, &d| acc.checked_mul(d))
        .ok_or_else(|| NiftiIoError::DimsTooLarge {
            path: path.to_path_buf(),
            dims: dims.to_vec(),
        })
}

fn read_unscaled(
    path: &Path,
    first_volume_only: bool,
) -> Result<(RawVolume, NiftiHeader), NiftiIoError> {
    let file = NiftiFile::open(path).map_err(|e| read_err(path, e))?;
    let hdr = file.header().clone();
    let mut dims = header_dims(path, &hdr)?;
    if first_volume_only && dims.len() > 4 {
        return Err(NiftiIoError::NotThreeD {
            path: path.to_path_buf(),
            ndim: dims.len() - 1,
        });
    }
    let datatype = file.datatype().map_err(|e| read_err(path, e))?;
    if first_volume_only && dims.len() == 4 {
        dims.truncate(3);
    }
    let want = volume_byte_len(path, &dims, datatype.size_of())?;
    let bytes = file.read_data(Some(want)).map_err(|e| read_err(path, e))?;
    let raw = RawVolume {
        bytes,
        datatype,
        big_endian: hdr.endianness == nifti::Endianness::Big,
        dims,
    };
    Ok((raw, hdr))
}

fn decode_c_order<T: Send + Copy + Default>(
    path: &Path,
    raw: &RawVolume,
    f: impl Fn(f64) -> T + Sync,
) -> Result<Vec<T>, NiftiIoError> {
    decode_map_c_order(&raw.bytes, raw.datatype, raw.big_endian, &raw.dims, f)
        .map_err(|e| read_err(path, e))
}

fn require_three_d(path: &Path, raw: &RawVolume) -> Result<[usize; 3], NiftiIoError> {
    match raw.dims[..] {
        [a, b, c] => Ok([a, b, c]),
        _ => Err(NiftiIoError::NotThreeD {
            path: path.to_path_buf(),
            ndim: raw.dims.len(),
        }),
    }
}

fn first_volume_any_layout<T>(path: &Path, mut data: ArrayD<T>) -> Result<Array3<T>, NiftiIoError> {
    if data.ndim() > 3 {
        let last = Axis(data.ndim() - 1);
        data = data.index_axis_move(last, 0);
    }
    let ndim = data.ndim();
    data.into_dimensionality::<Ix3>()
        .map_err(|_| NiftiIoError::NotThreeD {
            path: path.to_path_buf(),
            ndim,
        })
}

fn first_volume<T: Clone>(path: &Path, data: ArrayD<T>) -> Result<Array3<T>, NiftiIoError> {
    Ok(to_standard(first_volume_any_layout(path, data)?))
}

pub fn load_mask(path: &Path, threshold: f64) -> Result<(Array3<bool>, Mat4), NiftiIoError> {
    let (raw, hdr) = read_unscaled(path, true)?;
    let aff = fastwarp_core::nifti_affine::header_affine(&hdr);
    let scale = scaling(path, &hdr)?;
    let above = move |v: f64| match scale {
        Some((s, i)) => v * s + i > threshold,
        None => v > threshold,
    };
    let dims = require_three_d(path, &raw)?;
    let mask = decode_c_order(path, &raw, above)?;
    Ok((array3(path, dims, mask)?, aff))
}

fn array3<T>(path: &Path, dims: [usize; 3], data: Vec<T>) -> Result<Array3<T>, NiftiIoError> {
    Array3::from_shape_vec((dims[0], dims[1], dims[2]), data).map_err(|e| read_err(path, e))
}

pub fn load_volume_region_f64(
    path: &Path,
    region: impl FnOnce(&NiftiGrid) -> [Range<usize>; 3],
) -> Result<(Array3<f64>, NiftiGrid, [usize; 3]), NiftiIoError> {
    let (raw, hdr) = read_unscaled(path, true)?;
    let dims = header_dims(path, &hdr)?;
    if dims.len() < 3 {
        return Err(NiftiIoError::NotThreeD {
            path: path.to_path_buf(),
            ndim: dims.len(),
        });
    }
    let grid = NiftiGrid {
        dims: [dims[0], dims[1], dims[2]],
        affine: fastwarp_core::nifti_affine::header_affine(&hdr),
    };
    let r = region(&grid);
    let clip = |a: usize| {
        let end = r[a].end.min(grid.dims[a]);
        r[a].start.min(end)..end
    };
    let (ri, rj, rk) = (clip(0), clip(1), clip(2));
    let offset = [ri.start, rj.start, rk.start];
    let scale = scaling(path, &hdr)?;
    let raw_dims = require_three_d(path, &raw)?;
    let values = decode_c_order(path, &raw, |v| match scale {
        Some((s, i)) => v * s + i,
        None => v,
    })?;
    let full = array3(path, raw_dims, values)?;
    let data = full.slice(s![ri, rj, rk]).to_owned();
    Ok((to_standard(data), grid, offset))
}

pub fn read_grid(path: &Path) -> Result<NiftiGrid, NiftiIoError> {
    let hdr = NiftiHeader::from_file(path).map_err(|e| read_err(path, e))?;
    let dims = header_dims(path, &hdr)?;
    if dims.len() < 3 {
        return Err(NiftiIoError::NotThreeD {
            path: path.to_path_buf(),
            ndim: dims.len(),
        });
    }
    Ok(NiftiGrid {
        dims: [dims[0], dims[1], dims[2]],
        affine: fastwarp_core::nifti_affine::header_affine(&hdr),
    })
}

fn load_array_f64(
    path: &Path,
    first_volume_only: bool,
) -> Result<(ArrayD<f64>, NiftiHeader), NiftiIoError> {
    let (raw, hdr) = read_unscaled(path, first_volume_only)?;
    let scale = scaling(path, &hdr)?;
    let values = decode_c_order(path, &raw, |v| match scale {
        Some((s, i)) => v * s + i,
        None => v,
    })?;
    let data = ArrayD::from_shape_vec(IxDyn(&raw.dims), values).map_err(|e| read_err(path, e))?;
    Ok((data, hdr))
}

pub fn load_volume_f64(path: &Path) -> Result<(Array3<f64>, Mat4), NiftiIoError> {
    let (data, hdr) = load_array_f64(path, true)?;
    let aff = fastwarp_core::nifti_affine::header_affine(&hdr);
    Ok((first_volume(path, data)?, aff))
}

pub fn load_vector_field_f64(path: &Path) -> Result<Array4<f64>, NiftiIoError> {
    let (data, _) = load_array_f64(path, false)?;
    let ndim = data.ndim();
    data.into_dimensionality::<Ix4>()
        .map_err(|_| NiftiIoError::NotThreeD {
            path: path.to_path_buf(),
            ndim,
        })
}

fn nibabel_like_header(aff: &Mat4) -> NiftiHeader {
    let f = |r: usize| {
        [
            aff[r][0] as f32,
            aff[r][1] as f32,
            aff[r][2] as f32,
            aff[r][3] as f32,
        ]
    };
    let spacing = fastwarp_core::voxel_spacing_rowmajor(aff);
    let mut pixdim = [1.0f32; 8];
    for a in 0..3 {
        pixdim[a + 1] = spacing[a] as f32;
    }
    NiftiHeader {
        pixdim,
        sform_code: 2,
        qform_code: 0,
        srow_x: f(0),
        srow_y: f(1),
        srow_z: f(2),
        quatern_b: 0.0,
        quatern_c: 0.0,
        quatern_d: 0.0,
        quatern_x: aff[0][3] as f32,
        quatern_y: aff[1][3] as f32,
        quatern_z: aff[2][3] as f32,
        xyzt_units: 0,
        ..NiftiHeader::default()
    }
}

pub fn write_f32_4d(path: &Path, data: &Array4<f32>, aff: &Mat4) -> Result<(), NiftiIoError> {
    let hdr = nibabel_like_header(aff);
    WriterOptions::new(path)
        .reference_header(&hdr)
        .write_nifti(data)
        .map_err(|e| NiftiIoError::Write {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

pub fn write_u8_3d(path: &Path, data: &Array3<u8>, aff: &Mat4) -> Result<(), NiftiIoError> {
    let hdr = nibabel_like_header(aff);
    WriterOptions::new(path)
        .reference_header(&hdr)
        .write_nifti(data)
        .map_err(|e| NiftiIoError::Write {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_nifti_io_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_byte_length_that_overflows_is_an_error() {
        let p = Path::new("x.nii");
        assert_eq!(volume_byte_len(p, &[2, 3, 4], 8).unwrap(), 192);
        let e = volume_byte_len(p, &[usize::MAX / 2, 3], 4).unwrap_err();
        assert!(matches!(e, NiftiIoError::DimsTooLarge { .. }), "{e}");
    }

    #[test]
    fn f64_volume_round_trips_without_scaling_or_precision_loss() {
        let d = tmp("f64");
        let p = d.join("v.nii.gz");
        let data = Array3::from_shape_fn((5, 6, 7), |(i, j, k)| {
            0.1 * i as f64 + 1e-9 * j as f64 - 3.3 * k as f64
        });
        let hdr = NiftiHeader {
            sform_code: 1,
            srow_x: [-0.9375, 0.0, 0.0, 117.617],
            srow_y: [0.0, 0.9375, 0.0, -103.6465],
            srow_z: [0.0, 0.0, 1.5, -112.062],
            pixdim: [1.0, 0.9375, 0.9375, 1.5, 1.0, 1.0, 1.0, 1.0],
            ..NiftiHeader::default()
        };
        WriterOptions::new(&p)
            .reference_header(&hdr)
            .write_nifti(&data)
            .unwrap();
        let (back, aff) = load_volume_f64(&p).unwrap();
        assert_eq!(back, data);
        assert!(back.is_standard_layout());
        assert_eq!(aff[0][0], -0.9375);
        assert_eq!(aff[0][3], 117.617f32 as f64);
        let g = read_grid(&p).unwrap();
        assert_eq!(g.dims, [5, 6, 7]);
        assert_eq!(g.affine, aff);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn coordinate_image_header_matches_nibabel_defaults() {
        let d = tmp("coord");
        let p = d.join("c.nii.gz");
        let aff: Mat4 = [
            [8.0, 0.0, 0.0, -218.123456789],
            [0.0, 8.0, 0.0, -254.5],
            [0.0, 0.0, 8.0, -192.25],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let data = Array4::from_shape_fn((3, 4, 5, 3), |(i, j, k, c)| {
            (i + 10 * j + 100 * k + 1000 * c) as f32
        });
        write_f32_4d(&p, &data, &aff).unwrap();
        let hdr = NiftiHeader::from_file(&p).unwrap();
        assert_eq!(hdr.dim[..5], [4, 3, 4, 5, 3]);
        assert_eq!(hdr.sform_code, 2);
        assert_eq!(hdr.qform_code, 0);
        assert_eq!(hdr.pixdim[..5], [1.0, 8.0, 8.0, 8.0, 1.0]);
        assert_eq!(hdr.srow_x[3], -218.123456789f64 as f32);
        let back = load_vector_field_f64(&p).unwrap();
        assert_eq!(back[[2, 3, 4, 2]], (2 + 30 + 400 + 2000) as f64);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn zero_dimensions_and_masks() {
        let d = tmp("mask");
        let p = d.join("m.nii");
        let mask =
            Array3::from_shape_fn((4, 5, 6), |(i, j, k)| u8::from(i == 1 && j == 2 && k > 2));
        let aff: Mat4 = [
            [2.0, 0.0, 0.0, -4.0],
            [0.0, 2.0, 0.0, -5.0],
            [0.0, 0.0, 2.0, -6.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        write_u8_3d(&p, &mask, &aff).unwrap();
        let (m, a) = load_mask(&p, 0.5).unwrap();
        assert_eq!(m.dim(), (4, 5, 6));
        assert!(m.is_standard_layout());
        assert_eq!(m.iter().filter(|v| **v).count(), 3);
        assert!(m[[1, 2, 5]] && !m[[1, 2, 2]]);
        assert_eq!(a, aff);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[44..46].copy_from_slice(&0u16.to_le_bytes());
        let z = d.join("zero.nii");
        std::fs::write(&z, &bytes).unwrap();
        assert!(matches!(
            read_grid(&z),
            Err(NiftiIoError::EmptyDimension { .. })
        ));
        assert!(matches!(
            load_volume_f64(&z),
            Err(NiftiIoError::EmptyDimension { .. })
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn four_d_files_load_their_first_volume_and_regions_match_the_whole() {
        let d = tmp("first");
        let p = d.join("v4.nii.gz");
        let data = Array4::from_shape_fn((5, 6, 7, 3), |(i, j, k, t)| {
            (i + 10 * j + 100 * k + 1000 * t) as f32
        });
        let aff: Mat4 = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        write_f32_4d(&p, &data, &aff).unwrap();
        let (vol, _) = load_volume_f64(&p).unwrap();
        assert_eq!(vol.dim(), (5, 6, 7));
        assert_eq!(vol[[4, 5, 6]], 654.0);
        let (mask, _) = load_mask(&p, 300.0).unwrap();
        assert_eq!(mask, vol.mapv(|v| v > 300.0));
        let (part, grid, offset) =
            load_volume_region_f64(&p, |g| [1..4, 2..g.dims[1], 3..99]).unwrap();
        assert_eq!(grid.dims, [5, 6, 7]);
        assert_eq!(offset, [1, 2, 3]);
        assert_eq!(part, vol.slice(s![1..4, 2..6, 3..7]).to_owned());
        assert!(part.is_standard_layout());
        let five = d.join("v5.nii.gz");
        WriterOptions::new(&five)
            .write_nifti(&ndarray::Array5::<f32>::zeros((2, 2, 2, 1, 3)))
            .unwrap();
        assert!(matches!(
            load_volume_f64(&five),
            Err(NiftiIoError::NotThreeD { ndim: 4, .. })
        ));
        assert!(load_vector_field_f64(&p).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nan_slope_means_no_scaling() {
        let d = tmp("nan");
        let p = d.join("n.nii");
        let data = Array3::from_shape_fn((3, 3, 3), |(i, j, k)| (i + j + k) as i16);
        let hdr = NiftiHeader {
            sform_code: 2,
            srow_x: [1.0, 0.0, 0.0, 0.0],
            srow_y: [0.0, 1.0, 0.0, 0.0],
            srow_z: [0.0, 0.0, 1.0, 0.0],
            ..NiftiHeader::default()
        };
        WriterOptions::new(&p)
            .reference_header(&hdr)
            .write_nifti(&data)
            .unwrap();
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[112..116].copy_from_slice(&f32::NAN.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        let (back, _) = load_volume_f64(&p).unwrap();
        assert_eq!(back[[2, 2, 2]], 6.0);
        bytes[112..116].copy_from_slice(&2.0f32.to_le_bytes());
        bytes[116..120].copy_from_slice(&1.0f32.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        let (scaled, _) = load_volume_f64(&p).unwrap();
        assert_eq!(scaled[[2, 2, 2]], 13.0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
