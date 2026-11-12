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
// Portions derived from ITK `itkTransformIOBase.cxx` / `itkTransformBase.cxx` (transform type
// names and the parameter / fixed-parameter layout of each class). See
// `docs/THIRD_PARTY_PORTS.md`.

//! The ITK transform record shared by the `.mat`, `.txt` and `.h5` formats.

use crate::error::{unsupported, RegError, Result};
use crate::grid::Grid;
use crate::transform::affine::AffineTransform;
use crate::transform::composite::TransformElem;
use crate::transform::field::DisplacementField;

pub(crate) const FORMAT: &str = "ITK transform";
pub(crate) const AFFINE_NAME: &str = "AffineTransform_double_3_3";
pub(crate) const FIELD_NAME: &str = "DisplacementFieldTransform_double_3_3";
pub(crate) const COMPOSITE_NAME: &str = "CompositeTransform_double_3_3";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Record {
    pub type_name: String,
    pub parameters: Vec<f64>,
    pub fixed: Vec<f64>,
}

fn base_type(name: &str) -> Option<&str> {
    name.strip_suffix("_double_3_3")
        .or_else(|| name.strip_suffix("_float_3_3"))
}

pub(crate) fn is_composite(name: &str) -> bool {
    base_type(name) == Some("CompositeTransform")
}

pub(crate) fn is_displacement_field(name: &str) -> bool {
    base_type(name) == Some("DisplacementFieldTransform")
}

pub(crate) fn elem_from_record(r: &Record) -> Result<Option<TransformElem>> {
    let base = base_type(&r.type_name).ok_or_else(|| {
        unsupported(
            FORMAT,
            format!(
                "transform type {} (only 3-D double or float types)",
                r.type_name
            ),
        )
    })?;
    match base {
        "CompositeTransform" => Ok(None),
        "AffineTransform" | "MatrixOffsetTransformBase" | "Rigid3DTransform" => Ok(Some(
            TransformElem::Affine(AffineTransform::from_parameters(&r.parameters, &r.fixed)?),
        )),
        "Euler3DTransform" => Ok(Some(TransformElem::Affine(AffineTransform::from_euler3d(
            &r.parameters,
            &r.fixed,
        )?))),
        "TranslationTransform" => {
            if r.parameters.len() != 3 {
                return Err(RegError::Length {
                    what: "translation parameters",
                    expected: 3,
                    got: r.parameters.len(),
                });
            }
            Ok(Some(TransformElem::Affine(
                AffineTransform::from_translation([
                    r.parameters[0],
                    r.parameters[1],
                    r.parameters[2],
                ]),
            )))
        }
        "DisplacementFieldTransform" => {
            let grid = Grid::from_fixed_parameters(&r.fixed)?;
            let data: Vec<f32> = r.parameters.iter().map(|v| *v as f32).collect();
            Ok(Some(TransformElem::DisplacementField(
                DisplacementField::new(grid, data)?,
            )))
        }
        other => Err(unsupported(FORMAT, format!("transform class {other}"))),
    }
}

pub(crate) fn record_from_elem(e: &TransformElem) -> Record {
    match e {
        TransformElem::Affine(a) => Record {
            type_name: AFFINE_NAME.to_owned(),
            parameters: a.parameters().to_vec(),
            fixed: a.fixed_parameters().to_vec(),
        },
        TransformElem::DisplacementField(f) => Record {
            type_name: FIELD_NAME.to_owned(),
            parameters: f.data().iter().map(|v| f64::from(*v)).collect(),
            fixed: f.grid().fixed_parameters().to_vec(),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn affine_record_round_trip_and_type_handling() {
        let a = AffineTransform::new(
            [[1.0, 0.1, 0.0], [0.0, 1.0, 0.2], [0.3, 0.0, 1.0]],
            [1.0, 2.0, 3.0],
            [4.0, 5.0, 6.0],
        );
        let rec = record_from_elem(&TransformElem::Affine(a));
        assert_eq!(rec.type_name, AFFINE_NAME);
        assert_eq!(
            elem_from_record(&rec).unwrap(),
            Some(TransformElem::Affine(a))
        );
        let float = Record {
            type_name: "AffineTransform_float_3_3".into(),
            ..rec.clone()
        };
        assert!(elem_from_record(&float).unwrap().is_some());
        let comp = Record {
            type_name: COMPOSITE_NAME.into(),
            parameters: vec![],
            fixed: vec![],
        };
        assert_eq!(elem_from_record(&comp).unwrap(), None);
        let bad = Record {
            type_name: "AffineTransform_double_2_2".into(),
            ..rec.clone()
        };
        assert!(matches!(
            elem_from_record(&bad),
            Err(RegError::Unsupported { .. })
        ));
        let other = Record {
            type_name: "BSplineTransform_double_3_3".into(),
            ..rec
        };
        assert!(matches!(
            elem_from_record(&other),
            Err(RegError::Unsupported { .. })
        ));
    }

    #[test]
    fn euler3d_records_are_accepted() {
        let euler = Record {
            type_name: "Euler3DTransform_double_3_3".into(),
            parameters: vec![0.1, -0.2, 0.3, 1.0, 2.0, 3.0],
            fixed: vec![4.0, 5.0, 6.0],
        };
        let Some(TransformElem::Affine(a)) = elem_from_record(&euler).unwrap() else {
            panic!("expected an affine");
        };
        let want = AffineTransform::from_euler3d(&euler.parameters, &euler.fixed).unwrap();
        assert_eq!(a, want);
        let zyx = Record {
            fixed: vec![4.0, 5.0, 6.0, 1.0],
            ..euler
        };
        assert!(elem_from_record(&zyx).unwrap().is_some());
    }
}
