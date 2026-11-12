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
// Portions derived from ITK `itkHDF5TransformIO.cxx` / `itkHDF5TransformIO.h` (the dataset
// names and the group layout `TransformGroup/<k>/{TransformType, TransformFixedParameters,
// TransformParameters}`). See `docs/THIRD_PARTY_PORTS.md`.

//! ITK HDF5 transform files (`Composite.h5`, `InverseComposite.h5`).
//!
//! Layout, as written by ITK: root string datasets `HDFVersion`, `ITKVersion`, `OSName`,
//! `OSVersion`; group `TransformGroup` with member `0` holding only `TransformType` =
//! `CompositeTransform_double_3_3`, then members `1`, `2`, ... in queue order, each with
//! `TransformType`, `TransformFixedParameters` and `TransformParameters` (`f64` datasets).
//! A displacement field has 18 fixed parameters (size, origin, spacing, direction row-major)
//! and `3 * N` interleaved parameters. The low-level container is `hdf5.rs`.

use std::path::Path;

use crate::error::{fmt_err, RegError, Result};
use crate::grid::Grid;
use crate::transform::composite::{TransformChain, TransformElem};
use crate::transform::field::DisplacementField;
use crate::transform::hdf5::{write_bytes, H5File, Node};
use crate::transform::itk_params::{
    elem_from_record, is_displacement_field, record_from_elem, Record, COMPOSITE_NAME, FIELD_NAME,
    FORMAT,
};

const GROUP: &str = "TransformGroup";

/// The value written to `HDFVersion`.
pub const WRITTEN_HDF_VERSION: &str = "1.14.0";
/// The value written to `ITKVersion` (the ITK release whose file layout is reproduced).
pub const WRITTEN_ITK_VERSION: &str = "5.4.4";

fn read_chain(f: &H5File) -> Result<TransformChain> {
    let mut names: Vec<(u64, String)> = f
        .members(GROUP)?
        .into_iter()
        .map(|n| {
            n.parse::<u64>()
                .map(|k| (k, n.clone()))
                .map_err(|_| fmt_err(FORMAT, format!("non-numeric member `{n}` in {GROUP}")))
        })
        .collect::<Result<_>>()?;
    names.sort();
    let mut queue = Vec::new();
    for (_, name) in names {
        let base = format!("{GROUP}/{name}");
        let type_name = f.read_string(&format!("{base}/TransformType"))?;
        let params_path = format!("{base}/TransformParameters");
        if is_displacement_field(&type_name) && f.exists(&params_path) {
            let fixed = f.read_f64(&format!("{base}/TransformFixedParameters"))?;
            let grid = Grid::from_fixed_parameters(&fixed)?;
            let data = f.read_f32(&params_path)?;
            queue.push(TransformElem::DisplacementField(DisplacementField::new(
                grid, data,
            )?));
            continue;
        }
        let (parameters, fixed) = if f.exists(&params_path) {
            (
                f.read_f64(&params_path)?,
                f.read_f64(&format!("{base}/TransformFixedParameters"))?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let rec = Record {
            type_name,
            parameters,
            fixed,
        };
        if let Some(e) = elem_from_record(&rec)? {
            queue.push(e);
        }
    }
    if queue.is_empty() {
        return Err(fmt_err(FORMAT, "HDF5 transform file without a transform"));
    }
    Ok(TransformChain::from_queue(queue))
}

/// Parses the bytes of an ITK HDF5 transform file into a chain in queue order.
pub fn parse_h5(bytes: Vec<u8>) -> Result<TransformChain> {
    read_chain(&H5File::parse(bytes)?)
}

/// Reads an ITK HDF5 transform file.
pub fn read_h5_file(path: &Path) -> Result<TransformChain> {
    read_chain(&H5File::open(path)?)
}

/// Serialises a chain as an ITK HDF5 composite transform file: group `0` is the composite,
/// groups `1..` the queue in order.
pub fn format_h5(chain: &TransformChain) -> Result<Vec<u8>> {
    let mut groups: Vec<(String, Node)> = vec![(
        "0".to_owned(),
        Node::Group(vec![(
            "TransformType".to_owned(),
            Node::Str(COMPOSITE_NAME.to_owned()),
        )]),
    )];
    for (i, e) in chain.queue().iter().enumerate() {
        let (type_name, fixed, parameters) = match e {
            TransformElem::DisplacementField(f) => (
                FIELD_NAME.to_owned(),
                Node::F64(f.grid().fixed_parameters().to_vec()),
                Node::F32AsF64(f.data()),
            ),
            TransformElem::Affine(_) => {
                let rec = record_from_elem(e);
                (
                    rec.type_name,
                    Node::F64(rec.fixed),
                    Node::F64(rec.parameters),
                )
            }
        };
        groups.push((
            (i + 1).to_string(),
            Node::Group(vec![
                ("TransformType".to_owned(), Node::Str(type_name)),
                ("TransformFixedParameters".to_owned(), fixed),
                ("TransformParameters".to_owned(), parameters),
            ]),
        ));
    }
    if chain.is_empty() {
        return Err(RegError::Format {
            format: FORMAT,
            message: "cannot write an empty chain".into(),
        });
    }
    let root = vec![
        (
            "HDFVersion".to_owned(),
            Node::Str(WRITTEN_HDF_VERSION.to_owned()),
        ),
        (
            "ITKVersion".to_owned(),
            Node::Str(WRITTEN_ITK_VERSION.to_owned()),
        ),
        (
            "OSName".to_owned(),
            Node::Str(std::env::consts::OS.to_owned()),
        ),
        (
            "OSVersion".to_owned(),
            Node::Str(std::env::consts::ARCH.to_owned()),
        ),
        (GROUP.to_owned(), Node::Group(groups)),
    ];
    write_bytes(&root)
}

/// Writes a chain to an ITK HDF5 transform file.
pub fn write_h5_file(path: &Path, chain: &TransformChain) -> Result<()> {
    let bytes = format_h5(chain)?;
    std::fs::write(path, bytes).map_err(|source| RegError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::linalg::IDENTITY3;
    use crate::transform::affine::AffineTransform;
    use crate::transform::composite::TransformElem;
    use crate::transform::field::DisplacementField;

    const CTEST: &[u8] = include_bytes!("../../tests/data/ctest.h5");

    #[test]
    fn ants_sample_composite_is_affine_then_field() {
        let chain = parse_h5(CTEST.to_vec()).unwrap();
        assert_eq!(chain.len(), 2);
        let TransformElem::Affine(a) = &chain.queue()[0] else {
            panic!("first element must be the affine")
        };
        let TransformElem::DisplacementField(f) = &chain.queue()[1] else {
            panic!("second element must be the displacement field")
        };
        assert_eq!(
            a.matrix,
            [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        );
        assert_eq!(a.translation, [0.0; 3]);
        assert_eq!(a.center, [0.0; 3]);
        assert_eq!(f.grid().size(), [6, 5, 4]);
        assert_eq!(f.grid().spacing(), [1.0; 3]);
        assert_eq!(f.grid().origin(), [0.0; 3]);
        assert_eq!(f.grid().direction(), IDENTITY3);
        assert_eq!(f.data().len(), 360);
        assert!(f
            .data()
            .as_chunks::<3>()
            .0
            .iter()
            .all(|d| *d == [1.0, 0.0, 0.0]));
        assert_eq!(chain.apply([1.0, 1.0, 1.0]), [4.0, 1.0, 1.0]);
        assert_eq!(chain.apply([100.0, 1.0, 1.0]), [200.0, 1.0, 1.0]);
    }

    fn sample_chain() -> TransformChain {
        let g = Grid::new([5, 6, 7], [2.0, 2.5, 3.0], [-4.0, -5.0, -6.0], IDENTITY3).unwrap();
        let field = DisplacementField::from_fn(g, |p| {
            [0.01 * p[1] + 0.25, -0.02 * p[2], 0.005 * p[0] * p[1].sin()]
        });
        let affine = AffineTransform::new(
            [[1.01, 0.02, 0.0], [-0.02, 0.99, 0.01], [0.0, 0.0, 1.0]],
            [1.0, -2.0, 0.5],
            [3.0, 4.0, 5.0],
        );
        TransformChain::from_queue(vec![
            TransformElem::Affine(affine),
            TransformElem::DisplacementField(field),
        ])
    }

    #[test]
    fn write_read_round_trip_is_bit_exact() {
        let chain = sample_chain();
        let back = parse_h5(format_h5(&chain).unwrap()).unwrap();
        assert_eq!(back, chain);
    }

    #[test]
    fn streamed_field_parameters_give_the_same_bytes_as_f64_records() {
        let chain = sample_chain();
        let mut groups = vec![(
            "0".to_owned(),
            Node::Group(vec![(
                "TransformType".to_owned(),
                Node::Str(COMPOSITE_NAME.to_owned()),
            )]),
        )];
        for (i, e) in chain.queue().iter().enumerate() {
            let rec = record_from_elem(e);
            groups.push((
                (i + 1).to_string(),
                Node::Group(vec![
                    ("TransformType".to_owned(), Node::Str(rec.type_name)),
                    ("TransformFixedParameters".to_owned(), Node::F64(rec.fixed)),
                    ("TransformParameters".to_owned(), Node::F64(rec.parameters)),
                ]),
            ));
        }
        let root = vec![
            (
                "HDFVersion".to_owned(),
                Node::Str(WRITTEN_HDF_VERSION.to_owned()),
            ),
            (
                "ITKVersion".to_owned(),
                Node::Str(WRITTEN_ITK_VERSION.to_owned()),
            ),
            (
                "OSName".to_owned(),
                Node::Str(std::env::consts::OS.to_owned()),
            ),
            (
                "OSVersion".to_owned(),
                Node::Str(std::env::consts::ARCH.to_owned()),
            ),
            (GROUP.to_owned(), Node::Group(groups)),
        ];
        assert_eq!(format_h5(&chain).unwrap(), write_bytes(&root).unwrap());
        let f = H5File::parse(format_h5(&chain).unwrap()).unwrap();
        let TransformElem::DisplacementField(field) = &chain.queue()[1] else {
            panic!("second element must be the field")
        };
        let wide = f.read_f64("TransformGroup/2/TransformParameters").unwrap();
        let narrow = f.read_f32("TransformGroup/2/TransformParameters").unwrap();
        assert_eq!(narrow.as_slice(), field.data());
        assert!(wide.iter().zip(&narrow).all(|(w, n)| *w as f32 == *n));
    }

    #[test]
    fn written_files_follow_the_itk_dataset_names() {
        let bytes = format_h5(&sample_chain()).unwrap();
        let f = H5File::parse(bytes).unwrap();
        assert_eq!(
            f.members("").unwrap(),
            vec![
                "HDFVersion",
                "ITKVersion",
                "OSName",
                "OSVersion",
                "TransformGroup"
            ]
        );
        assert_eq!(f.members("TransformGroup").unwrap(), vec!["0", "1", "2"]);
        assert_eq!(
            f.members("TransformGroup/2").unwrap(),
            vec![
                "TransformType",
                "TransformFixedParameters",
                "TransformParameters"
            ]
        );
        assert_eq!(
            f.members("TransformGroup/0").unwrap(),
            vec!["TransformType"]
        );
        assert_eq!(
            f.read_f64("TransformGroup/1/TransformParameters")
                .unwrap()
                .len(),
            12
        );
        assert_eq!(
            f.read_f64("TransformGroup/2/TransformFixedParameters")
                .unwrap()
                .len(),
            18
        );
    }

    #[test]
    fn more_than_nine_members_sort_numerically() {
        let mut chain = TransformChain::new();
        for i in 0..11 {
            chain.push(TransformElem::Affine(AffineTransform::from_translation([
                f64::from(i),
                0.0,
                0.0,
            ])));
        }
        let back = parse_h5(format_h5(&chain).unwrap()).unwrap();
        assert_eq!(back, chain);
    }

    #[test]
    fn empty_chain_is_refused() {
        assert!(format_h5(&TransformChain::new()).is_err());
    }
}
