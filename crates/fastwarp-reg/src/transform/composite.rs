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
// Portions derived from ITK `itkCompositeTransform.hxx` and `itkMultiTransform.hxx`
// (`TransformPoint`: the queue is applied in reverse order; inverse = reversed queue of
// inverses). See `docs/THIRD_PARTY_PORTS.md`.

//! Transform elements and ITK composite chains.

use crate::error::{RegError, Result};
use crate::linalg::Vec3;
use crate::transform::affine::AffineTransform;
use crate::transform::field::DisplacementField;

/// One transform of a chain.
#[derive(Clone, Debug, PartialEq)]
pub enum TransformElem {
    /// A matrix-offset transform.
    Affine(AffineTransform),
    /// A dense displacement field.
    DisplacementField(DisplacementField),
}

impl TransformElem {
    /// Maps a point (LPS mm).
    pub fn apply(&self, p: Vec3) -> Vec3 {
        match self {
            TransformElem::Affine(a) => a.apply(p),
            TransformElem::DisplacementField(f) => f.transform_point(p),
        }
    }
}

/// An ITK `CompositeTransform`: a queue of transforms in the order they were added.
///
/// **The last transform added is applied first.** `[A, W]` therefore maps `p` to `A(W(p))`;
/// ANTs writes `Composite.h5` as `[affine, warp]` and `InverseComposite.h5` as
/// `[inverse warp, inverse affine]`. `antsApplyTransforms -t T1 -t T2` maps a reference point
/// through `T1` first (the queue `[T2, T1]`; measured with ANTs 2.6.5 in `tests/ants_compat.rs`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransformChain {
    queue: Vec<TransformElem>,
}

impl TransformChain {
    /// An empty chain (the identity).
    pub fn new() -> Self {
        Self::default()
    }

    /// A chain from a queue in ITK order (first element added first, applied last).
    pub fn from_queue(queue: Vec<TransformElem>) -> Self {
        Self { queue }
    }

    /// Adds a transform to the end of the queue; it becomes the first one applied.
    pub fn push(&mut self, elem: TransformElem) {
        self.queue.push(elem);
    }

    /// Appends all of `other`'s queue after this chain's queue, as ANTs does for a composite
    /// file given after other transforms.
    pub fn extend(&mut self, other: TransformChain) {
        self.queue.extend(other.queue);
    }

    /// The queue in ITK order.
    pub fn queue(&self) -> &[TransformElem] {
        &self.queue
    }

    /// Number of transforms.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the chain is the identity.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Maps a point (LPS mm): the queue is applied back to front.
    pub fn apply(&self, p: Vec3) -> Vec3 {
        self.queue.iter().rev().fold(p, |q, t| t.apply(q))
    }

    /// The inverse chain, what `antsApplyTransforms -t [file,1]` applies: the queue reversed and
    /// every element inverted. Only linear chains are invertible here; a displacement field (whose
    /// inverse ANTs does not compute either) or a singular matrix is [`RegError::NotInvertible`].
    pub fn inverse(&self) -> Result<TransformChain> {
        let queue = self
            .queue
            .iter()
            .rev()
            .map(|t| match t {
                TransformElem::Affine(a) => a.inverse().map(TransformElem::Affine),
                TransformElem::DisplacementField(_) => Err(RegError::NotInvertible(
                    "a displacement field has no closed-form inverse".into(),
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { queue })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn shift(t: Vec3) -> TransformElem {
        TransformElem::Affine(AffineTransform::from_translation(t))
    }

    fn scale2() -> TransformElem {
        TransformElem::Affine(AffineTransform::from_offset(
            [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]],
            [0.0; 3],
        ))
    }

    #[test]
    fn last_pushed_is_applied_first() {
        let mut c = TransformChain::new();
        c.push(shift([1.0, 0.0, 0.0]));
        c.push(scale2());
        assert_eq!(c.apply([1.0, 1.0, 1.0]), [3.0, 2.0, 2.0]);
        let mut d = TransformChain::new();
        d.push(scale2());
        d.push(shift([1.0, 0.0, 0.0]));
        assert_eq!(d.apply([1.0, 1.0, 1.0]), [4.0, 2.0, 2.0]);
        assert_eq!(
            TransformChain::new().apply([5.0, 6.0, 7.0]),
            [5.0, 6.0, 7.0]
        );
    }

    #[test]
    fn the_inverse_of_a_linear_chain_undoes_it_and_a_field_is_refused() {
        let rot = AffineTransform::new(
            [[0.9, -0.2, 0.05], [0.15, 1.1, 0.0], [0.0, 0.1, 0.95]],
            [1.5, -2.0, 0.25],
            [10.0, -4.0, 6.0],
        );
        let c = TransformChain::from_queue(vec![
            shift([1.0, 2.0, -3.0]),
            TransformElem::Affine(rot),
            scale2(),
        ]);
        let inv = c.inverse().unwrap();
        assert_eq!(inv.len(), 3);
        for p in [[0.0, 0.0, 0.0], [12.0, -7.0, 30.0]] {
            let back = inv.apply(c.apply(p));
            for a in 0..3 {
                assert!((back[a] - p[a]).abs() < 1e-12, "{back:?} {p:?}");
            }
        }
        assert!(TransformChain::new().inverse().unwrap().is_empty());
        let grid = crate::grid::Grid::new([2, 2, 2], [1.0; 3], [0.0; 3], crate::linalg::IDENTITY3)
            .unwrap();
        let field = TransformChain::from_queue(vec![
            shift([1.0, 0.0, 0.0]),
            TransformElem::DisplacementField(DisplacementField::zeros(grid)),
        ]);
        assert!(matches!(field.inverse(), Err(RegError::NotInvertible(_))));
        let singular =
            TransformChain::from_queue(vec![TransformElem::Affine(AffineTransform::from_offset(
                [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
                [0.0; 3],
            ))]);
        assert!(matches!(
            singular.inverse(),
            Err(RegError::NotInvertible(_))
        ));
    }
}
