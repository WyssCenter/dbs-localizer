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
// Portions derived from ITK `itkMatlabTransformIO.cxx` (a transform is a pair of MATLAB v4
// column vectors: the parameters named after the transform class, then `fixed`). See
// `docs/THIRD_PARTY_PORTS.md`.

//! ITK MATLAB Level-4 `.mat` transform files.
//!
//! Each variable is a 20-byte header of five little-endian `u32` (`type`, `rows`, `cols`,
//! `imag`, `name length` including the NUL), the NUL-terminated name, then `rows` values.
//! `type` 0 is `f64`, 10 is `f32`. A transform is two variables: the parameters, named after
//! the transform class (`AffineTransform_double_3_3`), and `fixed`. ANTs affine files are
//! exactly 193 bytes: `(20 + 27 + 96) + (20 + 6 + 24)`.

use std::path::Path;

use crate::error::{fmt_err, unsupported, RegError, Result};
use crate::transform::composite::TransformElem;
use crate::transform::itk_params::{
    elem_from_record, record_from_elem, Record, AFFINE_NAME, FORMAT,
};

const MAT: &str = "ITK MATLAB transform (.mat)";

struct Variable {
    name: String,
    values: Vec<f64>,
}

fn u32_at(b: &[u8], pos: usize) -> Result<u32> {
    b.get(pos..pos + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| fmt_err(MAT, "truncated header"))
}

fn read_variable(b: &[u8], pos: &mut usize) -> Result<Variable> {
    let ty = u32_at(b, *pos)?;
    let rows = u32_at(b, *pos + 4)? as usize;
    let cols = u32_at(b, *pos + 8)? as usize;
    let imag = u32_at(b, *pos + 12)?;
    let namlen = u32_at(b, *pos + 16)? as usize;
    if imag != 0 {
        return Err(unsupported(MAT, "complex variables"));
    }
    if cols != 1 {
        return Err(fmt_err(MAT, "only column-vector parameters are supported"));
    }
    let width = match ty {
        0 => 8,
        10 => 4,
        other => return Err(unsupported(MAT, format!("MATLAB v4 type code {other}"))),
    };
    let name_start = *pos + 20;
    let data_start = name_start + namlen;
    let data_end = rows
        .checked_mul(width)
        .and_then(|n| data_start.checked_add(n))
        .ok_or_else(|| fmt_err(MAT, "size overflow"))?;
    if namlen == 0 || data_end > b.len() {
        return Err(fmt_err(MAT, "truncated variable"));
    }
    let name_bytes = &b[name_start..data_start];
    let name =
        String::from_utf8_lossy(name_bytes.split(|c| *c == 0).next().unwrap_or(&[])).into_owned();
    let raw = &b[data_start..data_end];
    let values = if width == 8 {
        raw.as_chunks::<8>()
            .0
            .iter()
            .map(|&c| f64::from_le_bytes(c))
            .collect()
    } else {
        raw.as_chunks::<4>()
            .0
            .iter()
            .map(|&c| f64::from(f32::from_le_bytes(c)))
            .collect()
    };
    *pos = data_end;
    Ok(Variable { name, values })
}

/// Parses the bytes of a `.mat` transform file: one or more transforms.
pub fn parse_mat(bytes: &[u8]) -> Result<Vec<TransformElem>> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < bytes.len() {
        let params = read_variable(bytes, &mut pos)?;
        if pos >= bytes.len() {
            return Err(fmt_err(
                MAT,
                "missing `fixed` variable after the parameters",
            ));
        }
        let fixed = read_variable(bytes, &mut pos)?;
        if fixed.name != "fixed" {
            return Err(fmt_err(
                MAT,
                format!("expected variable `fixed`, found `{}`", fixed.name),
            ));
        }
        let rec = Record {
            type_name: params.name,
            parameters: params.values,
            fixed: fixed.values,
        };
        if let Some(e) = elem_from_record(&rec)? {
            out.push(e);
        }
    }
    if out.is_empty() {
        return Err(fmt_err(MAT, "no transform in file"));
    }
    Ok(out)
}

fn push_variable(out: &mut Vec<u8>, name: &str, values: &[f64]) {
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(values.len() as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(name.len() as u32 + 1).to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// Serialises affine transforms as `AffineTransform_double_3_3` / `fixed` variable pairs.
/// Displacement fields cannot be stored in a `.mat` file.
pub fn format_mat(elems: &[TransformElem]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for e in elems {
        let rec = record_from_elem(e);
        if rec.type_name != AFFINE_NAME {
            return Err(RegError::Unsupported {
                format: FORMAT,
                feature: format!("{} in a .mat file", rec.type_name),
            });
        }
        push_variable(&mut out, &rec.type_name, &rec.parameters);
        push_variable(&mut out, "fixed", &rec.fixed);
    }
    Ok(out)
}

/// Reads a `.mat` transform file.
pub fn read_mat_file(path: &Path) -> Result<Vec<TransformElem>> {
    let bytes = std::fs::read(path).map_err(|source| RegError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_mat(&bytes)
}

/// Writes affine transforms to a `.mat` file.
pub fn write_mat_file(path: &Path, elems: &[TransformElem]) -> Result<()> {
    let bytes = format_mat(elems)?;
    std::fs::write(path, bytes).map_err(|source| RegError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::transform::affine::AffineTransform;

    fn sample() -> AffineTransform {
        AffineTransform::new(
            [[0.9, 0.1, -0.05], [0.0, 1.1, 0.2], [0.03, -0.1, 1.0]],
            [1.0, -2.0, 3.0],
            [-5.9, 3.1, -46.7],
        )
    }

    #[test]
    fn round_trip_is_bit_exact_and_193_bytes() {
        let t = sample();
        let bytes = format_mat(&[TransformElem::Affine(t)]).unwrap();
        assert_eq!(bytes.len(), 193);
        let back = parse_mat(&bytes).unwrap();
        assert_eq!(back, vec![TransformElem::Affine(t)]);
    }

    #[test]
    fn float_variables_and_unknown_codes() {
        let t = sample();
        let mut b = Vec::new();
        for (name, vals) in [
            ("AffineTransform_float_3_3", t.parameters().to_vec()),
            ("fixed", t.fixed_parameters().to_vec()),
        ] {
            b.extend_from_slice(&10u32.to_le_bytes());
            b.extend_from_slice(&(vals.len() as u32).to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&(name.len() as u32 + 1).to_le_bytes());
            b.extend_from_slice(name.as_bytes());
            b.push(0);
            for v in vals {
                b.extend_from_slice(&(v as f32).to_le_bytes());
            }
        }
        let parsed = parse_mat(&b).unwrap();
        let TransformElem::Affine(a) = &parsed[0] else {
            panic!("not affine")
        };
        assert!((a.matrix[0][0] - 0.9).abs() < 1e-6);
        let mut bad = b.clone();
        bad[0] = 1;
        assert!(parse_mat(&bad).is_err());
        assert!(parse_mat(&b[..b.len() - 1]).is_err());
        assert!(parse_mat(&[]).is_err());
    }

    #[test]
    fn fields_are_refused() {
        let g = crate::grid::Grid::new([2, 2, 2], [1.0; 3], [0.0; 3], crate::linalg::IDENTITY3)
            .unwrap();
        let f =
            TransformElem::DisplacementField(crate::transform::field::DisplacementField::zeros(g));
        assert!(format_mat(&[f]).is_err());
    }
}
