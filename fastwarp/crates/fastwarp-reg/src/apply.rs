// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
// SPDX-FileCopyrightText: ANTs developers
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
// Portions derived from ITK `itkResampleImageFilter.hxx` (`NonlinearThreadedGenerateData`: output
// point, transform, continuous index, `IsInsideBuffer`, default pixel value) and from ANTs
// `antsApplyTransforms.cxx` (reference-grid resampling through a composite transform with linear
// or nearest-neighbour interpolation and default value zero). The nearest-neighbour index rule
// is ITK's `NearestNeighborInterpolateImageFunction` (`RoundHalfIntegerUp` of the continuous
// index). See `docs/THIRD_PARTY_PORTS.md`.

//! `antsApplyTransforms` on the CPU: an image resampled onto a reference grid through a
//! [`TransformChain`].

use std::path::Path;

use rayon::prelude::*;

use crate::error::Result;
use crate::grid::Grid;
use crate::interp::{is_inside_point, linear_at_index};
use crate::linalg::Vec3;
use crate::transform::TransformChain;
use crate::volume::Volume;

/// Interpolator of [`apply_chain`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// ITK `LinearInterpolateImageFunction`.
    Linear,
    /// ITK `NearestNeighborInterpolateImageFunction`.
    NearestNeighbor,
}

fn nearest_at_index(vol: &Volume<f64>, c: Vec3) -> f64 {
    let size = vol.grid().size();
    let mut idx = [0usize; 3];
    for a in 0..3 {
        let r = (c[a] + 0.5).floor();
        idx[a] = if r < 0.0 {
            0
        } else {
            (r as usize).min(size[a] - 1)
        };
    }
    vol.data()[vol.grid().linear_index(idx[0], idx[1], idx[2])]
}

/// Resamples `image` onto `reference`: reference voxel `x` takes the value of `image` at
/// `chain.apply(x)` (the last queue element acts first, as in `antsApplyTransforms -t ... -t ...`),
/// or `default_value` when that point lies outside the image buffer (continuous index outside
/// `[-0.5, size - 0.5)`). A displacement field leaves points outside its own buffer unchanged.
pub fn apply_chain(
    image: &Volume<f64>,
    reference: &Grid,
    chain: &TransformChain,
    interpolation: Interpolation,
    default_value: f64,
) -> Result<Volume<f64>> {
    let data: Vec<f64> = (0..reference.num_voxels())
        .into_par_iter()
        .map(|idx| {
            let [i, j, k] = reference.ijk(idx);
            let x = reference.index_to_point([i as f64, j as f64, k as f64]);
            let p = chain.apply(x);
            if !is_inside_point(image, p) {
                return default_value;
            }
            let c = image.grid().point_to_index(p);
            match interpolation {
                Interpolation::Linear => linear_at_index(image, c),
                Interpolation::NearestNeighbor => nearest_at_index(image, c),
            }
        })
        .collect();
    Volume::from_vec(*reference, data)
}

/// `antsApplyTransforms -i input -r reference -o output -t chain` on NIfTI files: `input` is
/// loaded as the first 3-D volume, resampled onto the grid of `reference`, and written as `f32`.
pub fn apply_nifti(
    input: &Path,
    reference: &Path,
    chain: &TransformChain,
    interpolation: Interpolation,
    default_value: f64,
    output: &Path,
) -> Result<()> {
    let image = Volume::<f32>::load_nifti(input)?.to_f64();
    let grid = crate::nifti_io::read_grid(reference)?;
    let resampled = apply_chain(&image, &grid, chain, interpolation, default_value)?;
    drop(image);
    let f32_data: Vec<f32> = resampled.data().iter().map(|v| *v as f32).collect();
    Volume::from_vec(grid, f32_data)?.save_nifti(output)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;
    use crate::transform::{AffineTransform, DisplacementField, TransformElem};

    fn grid(n: [usize; 3]) -> Grid {
        Grid::new(n, [2.0, 2.0, 2.0], [10.0, -4.0, 6.0], IDENTITY3).unwrap()
    }

    fn ramp(g: Grid) -> Volume<f64> {
        let data = (0..g.num_voxels())
            .map(|idx| {
                let [i, j, k] = g.ijk(idx);
                i as f64 + 10.0 * j as f64 + 100.0 * k as f64
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    }

    fn chain(elems: Vec<TransformElem>) -> TransformChain {
        TransformChain::from_queue(elems)
    }

    #[test]
    fn identity_chain_reproduces_the_image() {
        let g = grid([5, 4, 3]);
        let v = ramp(g);
        for interp in [Interpolation::Linear, Interpolation::NearestNeighbor] {
            let out = apply_chain(&v, &g, &TransformChain::new(), interp, 0.0).unwrap();
            for (a, b) in out.data().iter().zip(v.data()) {
                assert!((a - b).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn pure_affine_translation_samples_at_the_mapped_point() {
        let g = grid([6, 5, 4]);
        let v = ramp(g);
        let shift = AffineTransform::from_translation([3.0, 0.0, 0.0]);
        let c = chain(vec![TransformElem::Affine(shift)]);
        let out = apply_chain(&v, &g, &c, Interpolation::Linear, -7.0).unwrap();
        let at = |i, j, k| out.data()[g.linear_index(i, j, k)];
        assert!((at(1, 2, 1) - (2.5 + 20.0 + 100.0)).abs() < 1e-12);
        assert!((at(4, 2, 1) - (5.5 + 20.0 + 100.0)).abs() < 1e-12 || at(4, 2, 1) == -7.0);
        assert_eq!(at(5, 0, 0), -7.0);
        let nn = apply_chain(&v, &g, &c, Interpolation::NearestNeighbor, 0.0).unwrap();
        assert_eq!(nn.data()[g.linear_index(1, 2, 1)], 3.0 + 20.0 + 100.0);
        assert_eq!(nn.data()[g.linear_index(0, 0, 0)], 2.0);
    }

    #[test]
    fn nearest_neighbour_rounds_half_up_and_the_buffer_edge_is_half_a_voxel() {
        let g = grid([4, 1, 1]);
        let v = Volume::from_vec(g, vec![0.0, 10.0, 20.0, 30.0]).unwrap();
        let c = |dx: f64| {
            chain(vec![TransformElem::Affine(
                AffineTransform::from_translation([dx, 0.0, 0.0]),
            )])
        };
        let out = apply_chain(&v, &g, &c(1.0), Interpolation::NearestNeighbor, -1.0).unwrap();
        assert_eq!(out.data(), &[10.0, 20.0, 30.0, -1.0]);
        let out = apply_chain(&v, &g, &c(3.0), Interpolation::NearestNeighbor, -1.0).unwrap();
        assert_eq!(out.data(), &[20.0, 30.0, -1.0, -1.0]);
        let out = apply_chain(&v, &g, &c(-1.0), Interpolation::NearestNeighbor, -1.0).unwrap();
        assert_eq!(out.data(), &[0.0, 10.0, 20.0, 30.0]);
    }

    #[test]
    fn pure_field_displaces_the_sample_point_and_is_identity_outside_its_buffer() {
        let g = grid([6, 5, 4]);
        let v = ramp(g);
        let small = Grid::new(
            [3, 3, 3],
            [2.0; 3],
            g.index_to_point([1.0, 1.0, 1.0]),
            IDENTITY3,
        )
        .unwrap();
        let f = DisplacementField::from_fn(small, |_| [2.0, 0.0, 0.0]);
        let c = chain(vec![TransformElem::DisplacementField(f)]);
        let out = apply_chain(&v, &g, &c, Interpolation::Linear, 0.0).unwrap();
        let at = |i, j, k| out.data()[g.linear_index(i, j, k)];
        assert!((at(2, 2, 2) - (3.0 + 20.0 + 200.0)).abs() < 1e-9);
        assert!((at(0, 0, 0) - 0.0).abs() < 1e-12);
        assert!((at(5, 4, 3) - (5.0 + 40.0 + 300.0)).abs() < 1e-12);
    }

    #[test]
    fn queue_order_applies_the_last_element_first() {
        let g = grid([8, 3, 3]);
        let v = ramp(g);
        let shift = TransformElem::Affine(AffineTransform::from_translation([2.0, 0.0, 0.0]));
        let stretch = TransformElem::Affine(AffineTransform::new(
            [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
            g.origin(),
        ));
        let a = chain(vec![shift.clone(), stretch.clone()]);
        let b = chain(vec![stretch, shift]);
        let oa = apply_chain(&v, &g, &a, Interpolation::Linear, 0.0).unwrap();
        let ob = apply_chain(&v, &g, &b, Interpolation::Linear, 0.0).unwrap();
        let ia = oa.data()[g.linear_index(1, 1, 1)];
        let ib = ob.data()[g.linear_index(1, 1, 1)];
        assert!((ia - (3.0 + 10.0 + 100.0)).abs() < 1e-9);
        assert!((ib - (4.0 + 10.0 + 100.0)).abs() < 1e-9);
    }

    #[test]
    fn affine_then_field_matches_a_hand_computed_point() {
        let g = grid([8, 8, 8]);
        let v = ramp(g);
        let field = DisplacementField::from_fn(g, |_| [0.0, 2.0, 0.0]);
        let shift = AffineTransform::from_translation([4.0, 0.0, 0.0]);
        let c = chain(vec![
            TransformElem::Affine(shift),
            TransformElem::DisplacementField(field),
        ]);
        let out = apply_chain(&v, &g, &c, Interpolation::Linear, 0.0).unwrap();
        let got = out.data()[g.linear_index(2, 2, 2)];
        assert!((got - (4.0 + 30.0 + 200.0)).abs() < 1e-5);
    }

    #[test]
    fn apply_nifti_resamples_a_file_onto_the_reference_grid() {
        let g = grid([6, 5, 4]);
        let v = ramp(g);
        let dir =
            std::env::temp_dir().join(format!("fastwarp_reg_apply_nifti_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.nii.gz");
        let output = dir.join("out.nii.gz");
        let as_f32: Vec<f32> = v.data().iter().map(|x| *x as f32).collect();
        Volume::from_vec(g, as_f32)
            .unwrap()
            .save_nifti(&input)
            .unwrap();
        let shift = AffineTransform::from_translation([2.0, 0.0, 0.0]);
        let c = chain(vec![TransformElem::Affine(shift)]);
        apply_nifti(&input, &input, &c, Interpolation::Linear, 0.0, &output).unwrap();
        let back = Volume::<f32>::load_nifti(&output).unwrap();
        let direct = apply_chain(&v, &g, &c, Interpolation::Linear, 0.0).unwrap();
        assert!(back.grid().approx_eq(&g, 1e-6));
        for (a, b) in back.data().iter().zip(direct.data()) {
            assert!((f64::from(*a) - b).abs() < 1e-4);
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
