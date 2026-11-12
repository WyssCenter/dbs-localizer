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
// Portions derived from ITK `itkDisplacementFieldTransform.hxx` (`TransformPoint`: points outside
// the field buffer are returned unchanged) and `itkVectorLinearInterpolateImageFunction.hxx`
// (`EvaluateAtContinuousIndex`: floor base index, neighbours clamped to the buffer). See
// `docs/THIRD_PARTY_PORTS.md`.

//! Dense displacement fields with ITK `DisplacementFieldTransform` semantics.

use crate::error::{RegError, Result};
use crate::grid::Grid;
use crate::linalg::Vec3;

/// A displacement field on its own [`Grid`]: one `(dx, dy, dz)` per voxel in LPS millimetres,
/// interleaved, x-fastest. The transform maps a point `p` to `p + u(p)`.
///
/// `u` is interpolated trilinearly in physical space. ITK's rule for points outside the buffer
/// (continuous index outside `[-0.5, size - 0.5)` on any axis) applies: the point is returned
/// unchanged, i.e. the displacement is zero. Inside the half-voxel border the neighbours are
/// clamped to the buffer, so the displacement there equals the border voxel's.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplacementField {
    grid: Grid,
    data: Vec<f32>,
}

impl DisplacementField {
    /// Wraps interleaved `data` (3 values per voxel); fails on a length mismatch.
    pub fn new(grid: Grid, data: Vec<f32>) -> Result<Self> {
        let expected = grid.num_voxels() * 3;
        if data.len() != expected {
            return Err(RegError::Length {
                what: "displacement field data",
                expected,
                got: data.len(),
            });
        }
        Ok(Self { grid, data })
    }

    /// A zero field.
    pub fn zeros(grid: Grid) -> Self {
        let n = grid.num_voxels() * 3;
        Self {
            grid,
            data: vec![0.0; n],
        }
    }

    /// A field sampled from `f(physical point) -> displacement` at every voxel centre.
    pub fn from_fn(grid: Grid, f: impl Fn(Vec3) -> Vec3) -> Self {
        let mut data = Vec::with_capacity(grid.num_voxels() * 3);
        for linear in 0..grid.num_voxels() {
            let [i, j, k] = grid.ijk(linear);
            let u = f(grid.index_to_point([i as f64, j as f64, k as f64]));
            data.extend([u[0] as f32, u[1] as f32, u[2] as f32]);
        }
        Self { grid, data }
    }

    /// The field's grid.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Interleaved values, 3 per voxel, x-fastest.
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Mutable interleaved values.
    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// Displacement stored at voxel `(i, j, k)`; `None` outside the grid.
    pub fn get(&self, i: usize, j: usize, k: usize) -> Option<Vec3> {
        let [nx, ny, nz] = self.grid.size();
        if i >= nx || j >= ny || k >= nz {
            return None;
        }
        let o = self.grid.linear_index(i, j, k) * 3;
        Some([
            f64::from(self.data[o]),
            f64::from(self.data[o + 1]),
            f64::from(self.data[o + 2]),
        ])
    }

    /// Interpolated displacement at a physical point; `None` outside the ITK buffer.
    pub fn displacement_at(&self, p: Vec3) -> Option<Vec3> {
        let c = self.grid.point_to_index(p);
        if !self.grid.contains_continuous_index(c) {
            return None;
        }
        let size = self.grid.size();
        let mut base = [0i64; 3];
        let mut dist = [0.0f64; 3];
        for a in 0..3 {
            let f = c[a].floor();
            base[a] = f as i64;
            dist[a] = c[a] - f;
        }
        let mut out = [0.0f64; 3];
        for corner in 0..8usize {
            let mut overlap = 1.0f64;
            let mut idx = [0usize; 3];
            for a in 0..3 {
                let upper = (corner >> a) & 1 == 1;
                let (n, w) = if upper {
                    (base[a] + 1, dist[a])
                } else {
                    (base[a], 1.0 - dist[a])
                };
                idx[a] = n.clamp(0, size[a] as i64 - 1) as usize;
                overlap *= w;
            }
            if overlap != 0.0 {
                let o = self.grid.linear_index(idx[0], idx[1], idx[2]) * 3;
                for (k, v) in out.iter_mut().enumerate() {
                    *v += overlap * f64::from(self.data[o + k]);
                }
            }
        }
        Some(out)
    }

    /// `p + u(p)`, or `p` outside the field buffer.
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        match self.displacement_at(p) {
            Some(u) => [p[0] + u[0], p[1] + u[1], p[2] + u[2]],
            None => p,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    fn grid() -> Grid {
        Grid::new([6, 7, 8], [2.0, 2.0, 2.0], [-6.0, -7.0, -8.0], IDENTITY3).unwrap()
    }

    #[test]
    fn linear_field_is_reproduced_exactly_between_voxels() {
        let f = DisplacementField::from_fn(grid(), |p| {
            [0.1 * p[0] + 0.5, -0.05 * p[1], 0.02 * p[2] - 0.3]
        });
        let p = [-1.3, 0.7, 2.2];
        let u = f.displacement_at(p).unwrap();
        assert!((u[0] - (0.1 * p[0] + 0.5)).abs() < 1e-6);
        assert!((u[1] - (-0.05 * p[1])).abs() < 1e-6);
        assert!((u[2] - (0.02 * p[2] - 0.3)).abs() < 1e-6);
    }

    #[test]
    fn outside_the_buffer_the_point_is_unchanged() {
        let g = grid();
        let f = DisplacementField::from_fn(g, |_| [3.0, 4.0, 5.0]);
        let inside = g.index_to_point([2.0, 2.0, 2.0]);
        assert_eq!(
            f.transform_point(inside),
            [inside[0] + 3.0, inside[1] + 4.0, inside[2] + 5.0]
        );
        let far = [100.0, 0.0, 0.0];
        assert_eq!(f.transform_point(far), far);
        let just_outside = g.index_to_point([-0.51, 2.0, 2.0]);
        assert_eq!(f.transform_point(just_outside), just_outside);
    }

    #[test]
    fn half_voxel_border_uses_the_border_voxel() {
        let g = grid();
        let f = DisplacementField::from_fn(g, |p| [p[0], 0.0, 0.0]);
        let p = g.index_to_point([-0.4, 3.0, 3.0]);
        let u = f.displacement_at(p).unwrap();
        let border = f.get(0, 3, 3).unwrap();
        assert!((u[0] - border[0]).abs() < 1e-6);
        let q = g.index_to_point([5.4, 3.0, 3.0]);
        let u = f.displacement_at(q).unwrap();
        assert!((u[0] - f.get(5, 3, 3).unwrap()[0]).abs() < 1e-6);
    }

    #[test]
    fn length_is_checked() {
        assert!(DisplacementField::new(grid(), vec![0.0; 5]).is_err());
        assert_eq!(DisplacementField::zeros(grid()).data().len(), 6 * 7 * 8 * 3);
    }
}
