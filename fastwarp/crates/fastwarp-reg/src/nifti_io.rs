// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
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
// Portions derived from ITK `itkNiftiImageIO.cxx` (`SetImageIOOrientationFromNIfTI`): the rule
// that decides which of sform and qform defines the grid, and the RAS to LPS conversion of
// origin and direction. See `docs/THIRD_PARTY_PORTS.md`.

//! NIfTI geometry as ITK reads it, and NIfTI file I/O for [`Grid`] and [`Volume`].
//!
//! # Geometry rule (ITK `NiftiImageIO`)
//!
//! With both `qform_code` and `sform_code` zero the grid has origin 0 and identity direction.
//! Otherwise a 4x4 voxel-to-RAS matrix `M` is chosen: the sform is preferred when it is
//! (nearly) equal to the qform; or when it is orthonormal after removing the column scales and
//! either the qform code is zero, or the sform code is `1` (scanner anatomical), or both codes are
//! set and the two matrices agree within `1e-4`. Otherwise the qform is used. The grid is then
//! `origin = (-M03, -M13, M23)` and direction column `c` = `normalize(-M0c, -M1c, M2c)`, spacing
//! `|pixdim[1..=3]|` scaled by the length unit (metre x1000, micron x0.001). A grid therefore
//! keeps negative affine diagonals as direction signs, never as negative spacings.

use std::path::Path;

use fastwarp_core::nifti_affine::{voxel_to_world, NiftiXformFields};
use fastwarp_core::Mat4;
use fastwarp_niftiio::{decode_map, NiftiFile, NiftiFileError, WriterOptions};
use ndarray::{Array3, ArrayD, IxDyn, ShapeBuilder};
use nifti::{Endianness, NiftiHeader, NiftiType};

use crate::error::{RegError, Result};
use crate::grid::Grid;
use crate::linalg::{mat3_det, mat3_max_abs_diff, mat3_mul, mat3_transpose, Mat3, IDENTITY3};
use crate::volume::Volume;

/// Largest accepted voxel count per spatial axis of a NIfTI header (`rust-safety.md`).
pub const MAX_DIM: usize = 2048;

fn nifti_err(path: &Path, e: impl std::fmt::Display) -> RegError {
    RegError::Nifti {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

fn geom_err(path: &Path, message: impl Into<String>) -> RegError {
    RegError::NiftiGeometry {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn check_dims(path: &Path, h: &NiftiHeader) -> Result<Vec<usize>> {
    let ndim = usize::from(h.dim[0]).clamp(1, 7);
    let dims: Vec<usize> = (1..=ndim).map(|i| usize::from(h.dim[i])).collect();
    if dims.iter().take(3).any(|&d| d > MAX_DIM) {
        return Err(RegError::NiftiDimsTooLarge {
            path: path.to_path_buf(),
            dims,
        });
    }
    if dims.contains(&0) {
        return Err(geom_err(path, format!("zero-sized dimension in {dims:?}")));
    }
    Ok(dims)
}

fn length_unit_scale(h: &NiftiHeader) -> f64 {
    match h.xyzt_units & 7 {
        1 => 1e3,
        3 => 1e-3,
        _ => 1.0,
    }
}

fn sform_of(f: &NiftiXformFields) -> Mat4 {
    voxel_to_world(&NiftiXformFields {
        sform_code: 1,
        qform_code: 0,
        ..*f
    })
}

fn qform_of(f: &NiftiXformFields) -> Mat4 {
    voxel_to_world(&NiftiXformFields {
        sform_code: 0,
        qform_code: 1,
        ..*f
    })
}

fn rot3(m: &Mat4) -> Mat3 {
    let mut r = [[0.0; 3]; 3];
    for (i, row) in r.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = m[i][j];
        }
    }
    r
}

fn orthonormal_without_scale(m: &Mat4) -> bool {
    let mut r = rot3(m);
    for c in 0..3 {
        let n = (r[0][c] * r[0][c] + r[1][c] * r[1][c] + r[2][c] * r[2][c]).sqrt();
        if n == 0.0 || !n.is_finite() {
            return false;
        }
        for row in &mut r {
            row[c] /= n;
        }
    }
    mat3_max_abs_diff(&mat3_mul(&r, &mat3_transpose(&r)), &IDENTITY3) <= 1e-4
        && m[3][..3].iter().all(|v| *v == 0.0)
        && m[3][3] == 1.0
}

fn matrices_similar(s: &Mat4, q: &Mat4) -> bool {
    mat3_max_abs_diff(&rot3(s), &rot3(q)) <= 1e-5
        && (0..3).map(|r| (s[r][3] - q[r][3]).abs()).sum::<f64>() <= 1e-7
        && (0..4).map(|c| (s[3][c] - q[3][c]).abs()).sum::<f64>() <= 1e-7
}

fn matrices_very_similar(s: &Mat4, q: &Mat4) -> bool {
    mat3_max_abs_diff(&rot3(s), &rot3(q)) <= 1e-4
        && (0..3).all(|r| (s[r][3] - q[r][3]).abs() <= 1e-4)
}

fn chosen_matrix(path: &Path, h: &NiftiHeader, fields: &NiftiXformFields) -> Result<Mat4> {
    let s = sform_of(fields);
    let q = qform_of(fields);
    let mut prefer_sform = matrices_similar(&s, &q);
    if (!prefer_sform || h.sform_code != 0) && orthonormal_without_scale(&s) {
        if (h.qform_code == 0 && h.sform_code != 0) || h.sform_code == 1 {
            prefer_sform = true;
        } else if h.qform_code != 0 && h.sform_code != 0 {
            prefer_sform = matrices_very_similar(&s, &q);
        }
    }
    if prefer_sform && h.sform_code != 0 {
        Ok(s)
    } else if h.qform_code != 0 {
        Ok(q)
    } else {
        Err(geom_err(
            path,
            "no orthonormal sform or qform definition (ITK refuses such a header)",
        ))
    }
}

/// The grid ITK builds from a NIfTI header (see the module documentation for the rule).
pub fn grid_from_header(path: &Path, h: &NiftiHeader) -> Result<Grid> {
    let dims = check_dims(path, h)?;
    if dims.len() < 3 {
        return Err(geom_err(
            path,
            format!("expected at least 3 dimensions, got {}", dims.len()),
        ));
    }
    let scale = length_unit_scale(h);
    let spacing = [
        f64::from(h.pixdim[1]).abs() * scale,
        f64::from(h.pixdim[2]).abs() * scale,
        f64::from(h.pixdim[3]).abs() * scale,
    ];
    let size = [dims[0], dims[1], dims[2]];
    if h.qform_code == 0 && h.sform_code == 0 {
        return Grid::new(size, spacing, [0.0; 3], IDENTITY3)
            .map_err(|e| geom_err(path, e.to_string()));
    }
    let mut fields = NiftiXformFields::from(h);
    for a in 1..4 {
        fields.pixdim[a] = fields.pixdim[a].abs();
    }
    let m = chosen_matrix(path, h, &fields)?;
    let mut direction = [[0.0; 3]; 3];
    for c in 0..3 {
        let col = [-m[0][c], -m[1][c], m[2][c]];
        let n = (col[0] * col[0] + col[1] * col[1] + col[2] * col[2]).sqrt();
        if n == 0.0 {
            return Err(geom_err(
                path,
                "zero-length axis in the voxel-to-world matrix",
            ));
        }
        for r in 0..3 {
            direction[r][c] = col[r] / n;
        }
    }
    let origin = [-m[0][3] * scale, -m[1][3] * scale, m[2][3] * scale];
    Grid::new(size, spacing, origin, direction).map_err(|e| geom_err(path, e.to_string()))
}

fn quaternion(r: &Mat3) -> ([f32; 3], f32) {
    let mut m = *r;
    let qfac = if mat3_det(&m) < 0.0 {
        for row in &mut m {
            row[2] = -row[2];
        }
        -1.0f32
    } else {
        1.0f32
    };
    let (r11, r12, r13) = (m[0][0], m[0][1], m[0][2]);
    let (r21, r22, r23) = (m[1][0], m[1][1], m[1][2]);
    let (r31, r32, r33) = (m[2][0], m[2][1], m[2][2]);
    let a = r11 + r22 + r33 + 1.0;
    let (qa, mut qb, mut qc, mut qd);
    if a > 0.5 {
        qa = 0.5 * a.sqrt();
        qb = 0.25 * (r32 - r23) / qa;
        qc = 0.25 * (r13 - r31) / qa;
        qd = 0.25 * (r21 - r12) / qa;
    } else {
        let xd = 1.0 + r11 - (r22 + r33);
        let yd = 1.0 + r22 - (r11 + r33);
        let zd = 1.0 + r33 - (r11 + r22);
        if xd > 1.0 {
            qb = 0.5 * xd.sqrt();
            qa = 0.25 * (r32 - r23) / qb;
            qc = 0.25 * (r12 + r21) / qb;
            qd = 0.25 * (r13 + r31) / qb;
        } else if yd > 1.0 {
            qc = 0.5 * yd.sqrt();
            qb = 0.25 * (r12 + r21) / qc;
            qd = 0.25 * (r23 + r32) / qc;
            qa = 0.25 * (r13 - r31) / qc;
        } else {
            qd = 0.5 * zd.sqrt();
            qb = 0.25 * (r13 + r31) / qd;
            qc = 0.25 * (r23 + r32) / qd;
            qa = 0.25 * (r21 - r12) / qd;
        }
        if qa < 0.0 {
            qb = -qb;
            qc = -qc;
            qd = -qd;
        }
    }
    ([qb as f32, qc as f32, qd as f32], qfac)
}

/// A NIfTI header for `grid`: sform and qform both set to scanner anatomical (code 1) from the
/// grid's RAS affine, millimetre units, `pixdim` equal to the spacing.
pub fn header_from_grid(grid: &Grid) -> NiftiHeader {
    let m = grid.ras_affine();
    let row = |r: usize| {
        [
            m[r][0] as f32,
            m[r][1] as f32,
            m[r][2] as f32,
            m[r][3] as f32,
        ]
    };
    let mut rot = [[0.0; 3]; 3];
    let sp = grid.spacing();
    for r in 0..3 {
        for c in 0..3 {
            rot[r][c] = m[r][c] / sp[c];
        }
    }
    let (q, qfac) = quaternion(&rot);
    let mut pixdim = [1.0f32; 8];
    pixdim[0] = qfac;
    for a in 0..3 {
        pixdim[a + 1] = sp[a] as f32;
    }
    NiftiHeader {
        pixdim,
        sform_code: 1,
        qform_code: 1,
        srow_x: row(0),
        srow_y: row(1),
        srow_z: row(2),
        quatern_b: q[0],
        quatern_c: q[1],
        quatern_d: q[2],
        quatern_x: m[0][3] as f32,
        quatern_y: m[1][3] as f32,
        quatern_z: m[2][3] as f32,
        xyzt_units: 2,
        ..NiftiHeader::default()
    }
}

/// Reads only the header of `path` and returns its ITK grid.
pub fn read_grid(path: &Path) -> Result<Grid> {
    let h = NiftiHeader::from_file(path).map_err(|e| nifti_err(path, e))?;
    grid_from_header(path, &h)
}

fn scaling(path: &Path, h: &NiftiHeader) -> Result<Option<(f64, f64)>> {
    let slope = h.scl_slope;
    if slope == 0.0 || !slope.is_finite() {
        return Ok(None);
    }
    if !h.scl_inter.is_finite() {
        return Err(geom_err(path, "valid slope but invalid intercept"));
    }
    if slope == 1.0 && h.scl_inter == 0.0 {
        return Ok(None);
    }
    Ok(Some((f64::from(slope), f64::from(h.scl_inter))))
}

pub(crate) fn read_array_f64(path: &Path) -> Result<(NiftiHeader, ArrayD<f64>)> {
    let file = NiftiFile::open(path).map_err(|e| nifti_err(path, e))?;
    let h = file.header().clone();
    let dims = check_dims(path, &h)?;
    let datatype = file.datatype().map_err(|e| nifti_err(path, e))?;
    let scale = scaling(path, &h)?;
    let count = file.voxel_count().map_err(|e| nifti_err(path, e))?;
    let bytes = file.read_data(None).map_err(|e| nifti_err(path, e))?;
    let big_endian = matches!(h.endianness, Endianness::Big);
    let values = decode_values(path, datatype, big_endian, &bytes, count, scale, |v| v)?;
    let data = ArrayD::from_shape_vec(IxDyn(&dims).f(), values).map_err(|e| nifti_err(path, e))?;
    Ok((h, data))
}

fn decode_values<T: Send>(
    path: &Path,
    datatype: NiftiType,
    big_endian: bool,
    bytes: &[u8],
    count: usize,
    scale: Option<(f64, f64)>,
    conv: impl Fn(f64) -> T + Sync,
) -> Result<Vec<T>> {
    decode_map(bytes, datatype, big_endian, count, |v| match scale {
        Some((s, i)) => conv(v * s + i),
        None => conv(v),
    })
    .map_err(|e| match e {
        NiftiFileError::UnsupportedDataType(other) => geom_err(
            path,
            format!("unsupported data type {other:?} for a scalar volume"),
        ),
        other => nifti_err(path, other),
    })
}

fn read_first_volume<T: Send>(
    path: &Path,
    conv: impl Fn(f64) -> T + Sync,
) -> Result<(Grid, Vec<T>)> {
    let file = NiftiFile::open(path).map_err(|e| nifti_err(path, e))?;
    let h = file.header().clone();
    let dims = check_dims(path, &h)?;
    if dims.len() < 3 {
        return Err(geom_err(
            path,
            format!("expected a 3-D volume, shape {dims:?}"),
        ));
    }
    let grid = grid_from_header(path, &h)?;
    let datatype = file.datatype().map_err(|e| nifti_err(path, e))?;
    let scale = scaling(path, &h)?;
    let count = grid.num_voxels();
    let need = count
        .checked_mul(datatype.size_of())
        .ok_or_else(|| geom_err(path, "volume size overflows"))?;
    let bytes = file.read_data(Some(need)).map_err(|e| nifti_err(path, e))?;
    let big_endian = matches!(h.endianness, Endianness::Big);
    let data = decode_values(path, datatype, big_endian, &bytes, count, scale, conv)?;
    Ok((grid, data))
}

impl Volume<f32> {
    /// Loads a 3-D (or 4-D, first volume) NIfTI file as `f32`, applying `scl_slope` and
    /// `scl_inter`, with the geometry ITK derives from the header.
    pub fn load_nifti(path: &Path) -> Result<Self> {
        let (grid, data) = read_first_volume(path, |v| v as f32)?;
        Volume::from_vec(grid, data)
    }

    /// Writes the volume as an `f32` NIfTI file (gzip when the name ends in `.gz`) with both
    /// sform and qform set from the grid.
    pub fn save_nifti(&self, path: &Path) -> Result<()> {
        let [nx, ny, nz] = self.grid().size();
        let arr = Array3::from_shape_fn((nx, ny, nz), |(i, j, k)| {
            self.data()[self.grid().linear_index(i, j, k)]
        });
        let hdr = header_from_grid(self.grid());
        WriterOptions::new(path)
            .reference_header(&hdr)
            .write_nifti(&arr)
            .map_err(|e| nifti_err(path, e))
    }
}

impl Volume<f64> {
    pub fn load_nifti_f64(path: &Path) -> Result<Self> {
        let (grid, data) = read_first_volume(path, |v| f64::from(v as f32))?;
        Volume::from_vec(grid, data)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("fastwarp_reg_nifti_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn header(sform: Option<[[f32; 4]; 3]>, qform: i16, sform_code: i16) -> NiftiHeader {
        let mut h = NiftiHeader {
            dim: [3, 4, 5, 6, 1, 1, 1, 1],
            pixdim: [1.0, 2.0, 3.0, 4.0, 1.0, 1.0, 1.0, 1.0],
            xyzt_units: 2,
            qform_code: qform,
            sform_code,
            ..NiftiHeader::default()
        };
        if let Some(s) = sform {
            h.srow_x = s[0];
            h.srow_y = s[1];
            h.srow_z = s[2];
        }
        h
    }

    fn p() -> &'static Path {
        Path::new("t.nii")
    }

    #[test]
    fn ras_identity_sform_becomes_lps_flip_direction() {
        let h = header(
            Some([
                [2.0, 0.0, 0.0, 10.0],
                [0.0, 3.0, 0.0, 20.0],
                [0.0, 0.0, 4.0, 30.0],
            ]),
            0,
            1,
        );
        let g = grid_from_header(p(), &h).unwrap();
        assert_eq!(g.spacing(), [2.0, 3.0, 4.0]);
        assert_eq!(g.origin(), [-10.0, -20.0, 30.0]);
        assert_eq!(
            g.direction(),
            [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]
        );
        assert_eq!(g.size(), [4, 5, 6]);
    }

    #[test]
    fn negative_diagonal_keeps_spacing_positive_and_flips_direction_sign() {
        let h = header(
            Some([
                [-2.0, 0.0, 0.0, 90.0],
                [0.0, 3.0, 0.0, -100.0],
                [0.0, 0.0, -4.0, 50.0],
            ]),
            0,
            1,
        );
        let g = grid_from_header(p(), &h).unwrap();
        assert_eq!(g.spacing(), [2.0, 3.0, 4.0]);
        assert_eq!(g.origin(), [-90.0, 100.0, 50.0]);
        assert_eq!(
            g.direction(),
            [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]]
        );
        let ras = g.ras_affine();
        assert_eq!(ras[0][0], -2.0);
        assert_eq!(ras[2][2], -4.0);
    }

    #[test]
    fn oblique_sform_gives_normalised_columns_and_lps_origin() {
        let c = 0.4f64.cos() as f32;
        let s = 0.4f64.sin() as f32;
        let h = header(
            Some([
                [2.0 * c, -3.0 * s, 0.0, 1.0],
                [2.0 * s, 3.0 * c, 0.0, 2.0],
                [0.0, 0.0, 4.0, 3.0],
            ]),
            0,
            2,
        );
        let g = grid_from_header(p(), &h).unwrap();
        let d = g.direction();
        assert!((d[0][0] + f64::from(c)).abs() < 1e-6);
        assert!((d[1][0] + f64::from(s)).abs() < 1e-6);
        assert!((d[0][1] - f64::from(s)).abs() < 1e-6);
        assert!((d[1][1] + f64::from(c)).abs() < 1e-6);
        assert_eq!(d[2][2], 1.0);
        assert_eq!(g.origin(), [-1.0, -2.0, 3.0]);
    }

    #[test]
    fn qform_only_header_uses_the_quaternion_and_qfac() {
        let mut h = header(None, 1, 0);
        h.quatern_b = 0.0;
        h.quatern_c = 0.0;
        h.quatern_d = 0.0;
        h.quatern_x = 5.0;
        h.quatern_y = 6.0;
        h.quatern_z = 7.0;
        h.pixdim[0] = -1.0;
        let g = grid_from_header(p(), &h).unwrap();
        assert_eq!(g.origin(), [-5.0, -6.0, 7.0]);
        assert_eq!(
            g.direction(),
            [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]]
        );
        assert_eq!(g.spacing(), [2.0, 3.0, 4.0]);
    }

    #[test]
    fn no_codes_gives_identity_direction_and_zero_origin() {
        let h = header(None, 0, 0);
        let g = grid_from_header(p(), &h).unwrap();
        assert_eq!(g.origin(), [0.0; 3]);
        assert_eq!(g.direction(), IDENTITY3);
        assert_eq!(g.spacing(), [2.0, 3.0, 4.0]);
    }

    #[test]
    fn sform_that_disagrees_with_the_qform_loses_unless_scanner_anatomical() {
        let mut h = header(
            Some([
                [2.0, 0.0, 0.0, 10.0],
                [0.0, 3.0, 0.0, 20.0],
                [0.0, 0.0, 4.0, 30.0],
            ]),
            1,
            2,
        );
        h.quatern_x = -1.0;
        h.quatern_y = -2.0;
        h.quatern_z = -3.0;
        let g = grid_from_header(p(), &h).unwrap();
        assert_eq!(g.origin(), [1.0, 2.0, -3.0]);
        h.sform_code = 1;
        let g = grid_from_header(p(), &h).unwrap();
        assert_eq!(g.origin(), [-10.0, -20.0, 30.0]);
    }

    #[test]
    fn metre_units_scale_spacing_and_origin() {
        let mut h = header(
            Some([
                [0.002, 0.0, 0.0, 0.01],
                [0.0, 0.003, 0.0, 0.02],
                [0.0, 0.0, 0.004, 0.03],
            ]),
            0,
            1,
        );
        h.pixdim = [1.0, 0.002, 0.003, 0.004, 1.0, 1.0, 1.0, 1.0];
        h.xyzt_units = 1;
        let g = grid_from_header(p(), &h).unwrap();
        assert!((g.spacing()[0] - 2.0).abs() < 1e-6);
        assert!((g.origin()[2] - 30.0).abs() < 1e-4);
    }

    #[test]
    fn skewed_sform_without_qform_is_refused() {
        let h = header(
            Some([
                [2.0, 1.0, 0.0, 0.0],
                [0.0, 3.0, 0.0, 0.0],
                [0.0, 0.0, 4.0, 0.0],
            ]),
            0,
            1,
        );
        assert!(matches!(
            grid_from_header(p(), &h),
            Err(RegError::NiftiGeometry { .. })
        ));
    }

    #[test]
    fn header_round_trip_through_a_file_keeps_oblique_geometry() {
        let c = 0.5f64.cos();
        let s = 0.5f64.sin();
        let dir = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, -1.0]];
        let grid = Grid::new([3, 4, 5], [1.5, 2.0, 2.5], [-12.0, 33.0, -7.0], dir).unwrap();
        let data: Vec<f32> = (0..grid.num_voxels())
            .map(|i| i as f32 * 0.25 - 3.0)
            .collect();
        let vol = Volume::from_vec(grid, data.clone()).unwrap();
        let d = tmp("rt");
        for name in ["v.nii", "v.nii.gz"] {
            let path = d.join(name);
            vol.save_nifti(&path).unwrap();
            let back = Volume::<f32>::load_nifti(&path).unwrap();
            assert!(back.grid().approx_eq(&grid, 1e-5), "{:?}", back.grid());
            assert_eq!(back.data(), data.as_slice());
            assert!(read_grid(&path).unwrap().approx_eq(&grid, 1e-5));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn direct_decoding_matches_the_ndarray_conversion() {
        let d = tmp("decode");
        let mut h = header(
            Some([
                [2.0, 0.0, 0.0, 10.0],
                [0.0, 3.0, 0.0, 20.0],
                [0.0, 0.0, 4.0, 30.0],
            ]),
            0,
            1,
        );
        h.dim = [4, 4, 5, 6, 2, 1, 1, 1];
        h.scl_slope = 0.37;
        h.scl_inter = -11.25;
        let i16s = ndarray::Array4::from_shape_fn((4, 5, 6, 2), |(i, j, k, t)| {
            (i as i16 * 1009 - j as i16 * 313 + k as i16 * 77 - t as i16 * 5000) * 3
        });
        let f64s = ndarray::Array4::from_shape_fn((4, 5, 6, 2), |(i, j, k, t)| {
            (i as f64 * 0.1234567891 - j as f64 * 1e7 + k as f64 / 3.0) * (t as f64 + 1.0)
        });
        let u8s = ndarray::Array4::from_shape_fn((4, 5, 6, 2), |(i, j, k, t)| {
            (i * 61 + j * 17 + k * 5 + t * 101) as u8
        });
        let paths = [d.join("i16.nii"), d.join("f64.nii"), d.join("u8.nii")];
        let opts = |p: &std::path::PathBuf| WriterOptions::new(p).reference_header(&h);
        opts(&paths[0]).write_nifti(&i16s).unwrap();
        opts(&paths[1]).write_nifti(&f64s).unwrap();
        opts(&paths[2]).write_nifti(&u8s).unwrap();
        for path in &paths {
            let mut bytes = std::fs::read(path).unwrap();
            bytes[112..116].copy_from_slice(&0.37f32.to_le_bytes());
            bytes[116..120].copy_from_slice(&(-11.25f32).to_le_bytes());
            std::fs::write(path, bytes).unwrap();
        }
        for path in &paths {
            assert_eq!(read_grid(path).unwrap().size(), [4, 5, 6]);
            let (rh, all) = read_array_f64(path).unwrap();
            assert!(scaling(path, &rh).unwrap().is_some());
            let want: Vec<f32> = (0..6)
                .flat_map(|k| (0..5).flat_map(move |j| (0..4).map(move |i| (i, j, k))))
                .map(|(i, j, k)| all[[i, j, k, 0]] as f32)
                .collect();
            let got = Volume::<f32>::load_nifti(path).unwrap();
            assert_eq!(
                got.data().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                want.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
            let wide = Volume::<f64>::load_nifti_f64(path).unwrap();
            assert_eq!(wide, got.to_f64());
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn oversized_header_is_rejected_before_allocation() {
        let mut h = header(None, 0, 0);
        h.dim = [3, 4000, 5, 6, 1, 1, 1, 1];
        assert!(matches!(
            grid_from_header(p(), &h),
            Err(RegError::NiftiDimsTooLarge { .. })
        ));
    }
}
