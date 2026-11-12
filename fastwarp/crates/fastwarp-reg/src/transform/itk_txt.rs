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
// Portions derived from ITK `itkTxtTransformIO.cxx` (the `#Insight Transform File V1.0` layout:
// `Transform:`, `Parameters:`, `FixedParameters:` lines per transform). See
// `docs/THIRD_PARTY_PORTS.md`.

//! ITK text transform files (`#Insight Transform File V1.0`).
//!
//! Reader only.
//! The reader accepts any number of significant digits.

use std::path::Path;

use crate::error::{fmt_err, RegError, Result};
use crate::transform::composite::TransformElem;
use crate::transform::itk_params::{elem_from_record, is_composite, Record};

const TXT: &str = "ITK text transform";

fn numbers(line_value: &str) -> Result<Vec<f64>> {
    line_value
        .split_whitespace()
        .map(|t| {
            t.parse::<f64>()
                .map_err(|_| fmt_err(TXT, format!("bad number `{t}`")))
        })
        .collect()
}

/// Parses an ITK text transform file into its transforms, in file order. A leading
/// `CompositeTransform` record is skipped.
pub fn parse_text(text: &str) -> Result<Vec<TransformElem>> {
    let mut records: Vec<Record> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(fmt_err(TXT, format!("unrecognised line `{line}`")));
        };
        let value = value.trim();
        match key.trim() {
            "Transform" => records.push(Record {
                type_name: value.to_owned(),
                parameters: Vec::new(),
                fixed: Vec::new(),
            }),
            "Parameters" => {
                let r = records
                    .last_mut()
                    .ok_or_else(|| fmt_err(TXT, "`Parameters` before `Transform`"))?;
                r.parameters = numbers(value)?;
            }
            "FixedParameters" => {
                let r = records
                    .last_mut()
                    .ok_or_else(|| fmt_err(TXT, "`FixedParameters` before `Transform`"))?;
                r.fixed = numbers(value)?;
            }
            other => return Err(fmt_err(TXT, format!("unknown key `{other}`"))),
        }
    }
    let mut out = Vec::new();
    for r in &records {
        if is_composite(&r.type_name) {
            continue;
        }
        if let Some(e) = elem_from_record(r)? {
            out.push(e);
        }
    }
    if out.is_empty() {
        return Err(fmt_err(TXT, "no transform in file"));
    }
    Ok(out)
}

/// Reads an ITK text transform file.
pub fn read_text_file(path: &Path) -> Result<Vec<TransformElem>> {
    let text = std::fs::read_to_string(path).map_err(|source| RegError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_text(&text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::transform::affine::AffineTransform;

    #[test]
    fn multiple_transforms_are_read_in_file_order() {
        let a = AffineTransform::new(
            [
                [0.9396926208, -0.3420201433, 0.0],
                [0.3420201433, 0.9396926208, 0.0],
                [0.0, 0.0, 1.0],
            ],
            [1.25, -2.5, 0.125],
            [10.0, 20.0, 30.0],
        );
        let b = AffineTransform::from_translation([1.0, 2.0, 3.0]);
        let s =
            "#Insight Transform File V1.0\n#Transform 0\nTransform: AffineTransform_double_3_3\n\
                 Parameters: 0.939692620800 -0.342020143300 0.000000000000 0.342020143300 \
                 0.939692620800 0.000000000000 0.000000000000 0.000000000000 1.000000000000 \
                 1.250000000000 -2.500000000000 0.125000000000\n\
                 FixedParameters: 10.000000000000 20.000000000000 30.000000000000\n\
                 #Transform 1\nTransform: AffineTransform_double_3_3\n\
                 Parameters: 1 0 0 0 1 0 0 0 1 1 2 3\nFixedParameters: 0 0 0\n";
        let back = parse_text(s).unwrap();
        assert_eq!(back.len(), 2);
        let TransformElem::Affine(a2) = &back[0] else {
            panic!("not affine")
        };
        for r in 0..3 {
            for c in 0..3 {
                assert!((a2.matrix[r][c] - a.matrix[r][c]).abs() < 1e-12);
            }
        }
        assert_eq!(back[1], TransformElem::Affine(b));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse_text("").is_err());
        assert!(parse_text("Parameters: 1 2 3\n").is_err());
        assert!(parse_text("Transform: AffineTransform_double_3_3\nParameters: 1 x\n").is_err());
        assert!(parse_text("garbage\n").is_err());
    }

    #[test]
    fn composite_header_is_skipped() {
        let text = "#Insight Transform File V1.0\n#Transform 0\nTransform: CompositeTransform_double_3_3\n\
                    #Transform 1\nTransform: AffineTransform_double_3_3\n\
                    Parameters: 1 0 0 0 1 0 0 0 1 1 2 3\nFixedParameters: 0 0 0\n";
        let v = parse_text(text).unwrap();
        assert_eq!(v.len(), 1);
    }
}
