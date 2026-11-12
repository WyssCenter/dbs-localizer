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
// Portions derived from ITK `itkImageMaskSpatialObject.hxx`
// (`IsInsideInObjectSpace`: nearest voxel of the mask image in world space, buffered region
// test, non-zero pixel) and `itkImageToImageMetricv4.hxx`
// (`TransformAndEvaluateFixedPoint`: a fixed point outside the fixed mask contributes nothing).
// See `docs/THIRD_PARTY_PORTS.md`.

//! Fixed-image registration masks with ITK `ImageMaskSpatialObject` semantics.

use crate::error::{RegError, Result};
use crate::grid::Grid;
use crate::linalg::Vec3;
use crate::volume::Volume;

/// A fixed-image mask: the inside test of ITK's `ImageMaskSpatialObject` on a background-zero
/// mask image, evaluated in physical (LPS mm) space.
///
/// The mask grid need not be the fixed image grid; the point is mapped through each grid on its
/// own, exactly as ANTs attaches the `--masks [fixed,NULL]` image through a spatial object.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskVolume {
    grid: Grid,
    data: Vec<f64>,
}

impl MaskVolume {
    /// Wraps the mask values on their grid; fails when the length is not the voxel count.
    pub fn new(grid: Grid, data: Vec<f64>) -> Result<Self> {
        if data.len() != grid.num_voxels() {
            return Err(RegError::Length {
                what: "mask volume data",
                expected: grid.num_voxels(),
                got: data.len(),
            });
        }
        Ok(Self { grid, data })
    }

    /// A mask from any scalar volume; a voxel is foreground when its value is not zero.
    pub fn from_volume(volume: Volume<f64>) -> Self {
        let (grid, data) = volume.into_parts();
        Self { grid, data }
    }

    /// The mask grid.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// The mask values, x-fastest.
    pub fn data(&self) -> &[f64] {
        &self.data
    }

    /// ITK `ImageMaskSpatialObject::IsInsideInWorldSpace` for a background-zero mask: the nearest
    /// voxel (round half up) of the mask image must be inside the buffer and non-zero.
    pub fn is_inside(&self, p: Vec3) -> bool {
        let c = self.grid.point_to_index(p);
        let size = self.grid.size();
        let mut index = [0usize; 3];
        for a in 0..3 {
            let r = (c[a] + 0.5).floor();
            if !(r >= 0.0 && r < size[a] as f64) {
                return false;
            }
            index[a] = r as usize;
        }
        self.data[self.grid.linear_index(index[0], index[1], index[2])] != 0.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    fn rot_z(angle: f64) -> crate::linalg::Mat3 {
        let (s, c) = angle.sin_cos();
        [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
    }

    #[test]
    fn nearest_voxel_rule_and_buffer_edge() {
        let g = Grid::new([4, 4, 4], [2.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let mut data = vec![0.0; g.num_voxels()];
        data[g.linear_index(1, 1, 1)] = 7.0;
        let m = MaskVolume::new(g, data).unwrap();
        assert!(m.is_inside(g.index_to_point([1.0, 1.0, 1.0])));
        assert!(m.is_inside(g.index_to_point([1.4, 1.0, 1.0])));
        assert!(!m.is_inside(g.index_to_point([1.6, 1.0, 1.0])));
        assert!(!m.is_inside(g.index_to_point([8.1, 0.0, 0.0])));
        assert!(!m.is_inside([f64::NAN, 0.0, 0.0]));
    }

    #[test]
    fn inside_test_uses_physical_space_on_an_oblique_grid() {
        let g = Grid::new([6, 6, 6], [1.0; 3], [-3.0, -3.0, -3.0], rot_z(0.4)).unwrap();
        let mut data = vec![0.0; g.num_voxels()];
        data[g.linear_index(3, 0, 0)] = 1.0;
        let m = MaskVolume::new(g, data).unwrap();
        let centre = g.index_to_point([3.0, 0.0, 0.0]);
        assert!(m.is_inside(centre));
        let far = [centre[0] + 2.0, centre[1] + 2.0, centre[2]];
        assert!(!m.is_inside(far));
        assert!(MaskVolume::new(g, vec![1.0; 3]).is_err());
    }
}
