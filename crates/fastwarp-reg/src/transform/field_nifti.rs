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
// Portions derived from ITK `itkNiftiImageIO.cxx` (vector images: `dim = [5, nx, ny, nz, 1, 3]`,
// intent codes `NIFTI_INTENT_VECTOR` / `NIFTI_INTENT_DISPVECT`, and which of them has its x and y
// components sign-flipped when converting RAS to LPS). See `docs/THIRD_PARTY_PORTS.md`.

//! ANTs displacement-field NIfTI files (`1Warp.nii.gz`, `1InverseWarp.nii.gz`).
//!
//! On disk: `dim = [5, nx, ny, nz, 1, 3]`, the component index slowest, `intent_code = 1007`
//! (`NIFTI_INTENT_VECTOR`), the sform and qform of the fixed grid. The vector components are ITK
//! LPS millimetres stored as they are: ITK flips the geometry from RAS to LPS but only flips the
//! x and y components of files with intent `1006` (`NIFTI_INTENT_DISPVECT`). This module writes
//! `1007`, never `1006`, reads both, and flips the components of a `1006` file on load.

use std::path::Path;

use fastwarp_niftiio::WriterOptions;
use ndarray::Array5;

use crate::error::{RegError, Result};
use crate::nifti_io::{grid_from_header, header_from_grid, read_array_f64};
use crate::transform::field::DisplacementField;

/// `NIFTI_INTENT_VECTOR`.
pub const INTENT_VECTOR: i16 = 1007;
/// `NIFTI_INTENT_DISPVECT`.
pub const INTENT_DISPVECT: i16 = 1006;

/// Reads a displacement-field NIfTI file (`f32` or `f64` on disk, shape `[nx, ny, nz, 3]` or
/// `[nx, ny, nz, 1, 3]`) into an `f32` field.
pub fn read_field_nifti(path: &Path) -> Result<DisplacementField> {
    let (h, data) = read_array_f64(path)?;
    let grid = grid_from_header(path, &h)?;
    let shape = data.shape().to_vec();
    let ok = match shape.len() {
        4 => shape[3] == 3,
        5 => shape[3] == 1 && shape[4] == 3,
        _ => false,
    };
    if !ok {
        return Err(RegError::NiftiGeometry {
            path: path.to_path_buf(),
            message: format!("not a displacement field: shape {shape:?}"),
        });
    }
    let flip = if h.intent_code == INTENT_DISPVECT {
        -1.0
    } else {
        1.0
    };
    let [nx, ny, nz] = grid.size();
    let mut out = Vec::with_capacity(grid.num_voxels() * 3);
    let mut idx = vec![0usize; shape.len()];
    let last = shape.len() - 1;
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                idx[0] = i;
                idx[1] = j;
                idx[2] = k;
                for c in 0..3 {
                    idx[last] = c;
                    let s = if c < 2 { flip } else { 1.0 };
                    out.push((s * data[idx.as_slice()]) as f32);
                }
            }
        }
    }
    DisplacementField::new(grid, out)
}

/// Writes a displacement field as `f32`, `dim = [5, nx, ny, nz, 1, 3]`, intent `1007`.
pub fn write_field_nifti(path: &Path, field: &DisplacementField) -> Result<()> {
    let g = field.grid();
    let [nx, ny, nz] = g.size();
    let arr = Array5::from_shape_fn((nx, ny, nz, 1, 3), |(i, j, k, _, c)| {
        field.data()[g.linear_index(i, j, k) * 3 + c]
    });
    let mut hdr = header_from_grid(g);
    hdr.intent_code = INTENT_VECTOR;
    WriterOptions::new(path)
        .reference_header(&hdr)
        .write_nifti(&arr)
        .map_err(|e| RegError::Nifti {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use nifti::NiftiHeader;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fastwarp_reg_fieldnii_{}_{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn field() -> DisplacementField {
        let c = 0.2f64.cos();
        let s = 0.2f64.sin();
        let g = Grid::new(
            [4, 5, 6],
            [2.0, 2.0, 3.0],
            [-3.0, 4.0, -5.0],
            [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
        )
        .unwrap();
        DisplacementField::from_fn(g, |p| [p[0] * 0.1, -p[1] * 0.2 + 1.0, p[2] * 0.05 - 0.5])
    }

    #[test]
    fn round_trip_keeps_values_geometry_and_header_layout() {
        let d = tmp("rt");
        let p = d.join("1Warp.nii.gz");
        let f = field();
        write_field_nifti(&p, &f).unwrap();
        let h = NiftiHeader::from_file(&p).unwrap();
        assert_eq!(h.dim[..6], [5, 4, 5, 6, 1, 3]);
        assert_eq!(h.intent_code, INTENT_VECTOR);
        let back = read_field_nifti(&p).unwrap();
        assert!(back.grid().approx_eq(f.grid(), 1e-5));
        assert_eq!(back.data(), f.data());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn intent_1006_flips_x_and_y_on_load() {
        let d = tmp("flip");
        let p = d.join("w.nii");
        let f = field();
        let g = f.grid();
        let [nx, ny, nz] = g.size();
        let arr = Array5::from_shape_fn((nx, ny, nz, 1, 3), |(i, j, k, _, c)| {
            f.data()[g.linear_index(i, j, k) * 3 + c]
        });
        let mut hdr = header_from_grid(g);
        hdr.intent_code = INTENT_DISPVECT;
        WriterOptions::new(&p)
            .reference_header(&hdr)
            .write_nifti(&arr)
            .unwrap();
        let back = read_field_nifti(&p).unwrap();
        let a = f.get(1, 2, 3).unwrap();
        let b = back.get(1, 2, 3).unwrap();
        assert_eq!([b[0], b[1], b[2]], [-a[0], -a[1], a[2]]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scalar_volumes_are_not_fields() {
        let d = tmp("scalar");
        let p = d.join("s.nii");
        let g = *field().grid();
        crate::volume::Volume::<f32>::zeros(g)
            .save_nifti(&p)
            .unwrap();
        assert!(matches!(
            read_field_nifti(&p),
            Err(RegError::NiftiGeometry { .. })
        ));
        let _ = std::fs::remove_dir_all(&d);
    }
}
