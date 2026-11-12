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
// Portions derived from ITK `itkLinearInterpolateImageFunction.h` / `.hxx` (`EvaluateOptimized`
// for three dimensions: base index clamped to the first voxel, no interpolation across an axis
// whose distance is not positive or whose upper neighbour lies beyond the last voxel),
// `itkImageFunction.hxx` (`IsInsideBuffer` on the continuous index) and
// `itkCentralDifferenceImageFunction.hxx` (`Evaluate` at a point: offsets of half a spacing along
// the physical axes, zero component when either offset point is outside the buffer). See
// `docs/THIRD_PARTY_PORTS.md`.

//! Linear interpolation and central-difference gradients on a [`Volume`], with ITK's buffer rules.

use crate::linalg::Vec3;
use crate::volume::Volume;

/// ITK `ImageFunction::IsInsideBuffer` for a physical point: the continuous index lies in
/// `[-0.5, size - 0.5)` on every axis.
pub fn is_inside_point(vol: &Volume<f64>, p: Vec3) -> bool {
    vol.grid()
        .contains_continuous_index(vol.grid().point_to_index(p))
}

/// ITK `LinearInterpolateImageFunction::EvaluateAtContinuousIndex` (no bounds check; indices in
/// the half-voxel border read the edge voxel).
pub fn linear_at_index(vol: &Volume<f64>, c: Vec3) -> f64 {
    let size = vol.grid().size();
    let data = vol.data();
    let mut base = [0usize; 3];
    let mut weight = [0.0f64; 3];
    for a in 0..3 {
        let floor = c[a].floor();
        let b = if floor < 0.0 { 0 } else { floor as usize };
        let b = b.min(size[a] - 1);
        let d = c[a] - b as f64;
        base[a] = b;
        weight[a] = if d <= 0.0 || b + 1 > size[a] - 1 {
            0.0
        } else {
            d
        };
    }
    let at = |dx: usize, dy: usize, dz: usize| -> f64 {
        data[vol
            .grid()
            .linear_index(base[0] + dx, base[1] + dy, base[2] + dz)]
    };
    let lerp = |a: f64, b: f64, w: f64| a + (b - a) * w;
    let x = |dy: usize, dz: usize| -> f64 {
        let v0 = at(0, dy, dz);
        if weight[0] > 0.0 {
            lerp(v0, at(1, dy, dz), weight[0])
        } else {
            v0
        }
    };
    let y = |dz: usize| -> f64 {
        let v0 = x(0, dz);
        if weight[1] > 0.0 {
            lerp(v0, x(1, dz), weight[1])
        } else {
            v0
        }
    };
    let v0 = y(0);
    if weight[2] > 0.0 {
        lerp(v0, y(1), weight[2])
    } else {
        v0
    }
}

/// Linear interpolation at a physical point; `None` outside the buffer.
pub fn linear_at_point(vol: &Volume<f64>, p: Vec3) -> Option<f64> {
    let c = vol.grid().point_to_index(p);
    vol.grid()
        .contains_continuous_index(c)
        .then(|| linear_at_index(vol, c))
}

/// ITK `CentralDifferenceImageFunction::Evaluate` with `UseImageDirection` on, on a linearly
/// interpolated image: component `d` is `(I(p + h_d e_d) - I(p - h_d e_d)) / (2 h_d)` with
/// `h_d` half the image spacing of axis `d`, along the physical axis `e_d`; zero when either
/// offset point is outside the buffer.
pub fn central_difference_at_point(vol: &Volume<f64>, p: Vec3) -> Vec3 {
    let spacing = vol.grid().spacing();
    let mut out = [0.0; 3];
    for d in 0..3 {
        let offset = 0.5 * spacing[d];
        let mut p1 = p;
        p1[d] = p[d] - offset;
        let mut p2 = p;
        p2[d] = p[d] + offset;
        let (Some(v1), Some(v2)) = (linear_at_point(vol, p1), linear_at_point(vol, p2)) else {
            continue;
        };
        let delta = p2[d] - p1[d];
        if delta > 10.0 * f64::EPSILON {
            out[d] = (v2 - v1) / delta;
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::linalg::IDENTITY3;

    fn ramp() -> Volume<f64> {
        let g = Grid::new([5, 4, 3], [2.0, 1.5, 1.0], [1.0, -2.0, 3.0], IDENTITY3).unwrap();
        let mut data = vec![0.0; g.num_voxels()];
        for k in 0..3 {
            for j in 0..4 {
                for i in 0..5 {
                    data[g.linear_index(i, j, k)] =
                        2.0 * i as f64 - 3.0 * j as f64 + 0.5 * k as f64;
                }
            }
        }
        Volume::from_vec(g, data).unwrap()
    }

    #[test]
    fn linear_reproduces_an_affine_image_and_clamps_the_half_voxel_border() {
        let v = ramp();
        let c = [1.25, 2.5, 0.75];
        let want = 2.0 * c[0] - 3.0 * c[1] + 0.5 * c[2];
        assert!((linear_at_index(&v, c) - want).abs() < 1e-12);
        assert_eq!(linear_at_index(&v, [-0.4, 0.0, 0.0]), 0.0);
        assert_eq!(linear_at_index(&v, [4.4, 0.0, 0.0]), 8.0);
        assert!(linear_at_point(&v, v.grid().index_to_point([-0.6, 0.0, 0.0])).is_none());
        assert!(linear_at_point(&v, v.grid().index_to_point([4.49, 3.0, 2.0])).is_some());
    }

    #[test]
    fn central_difference_is_the_physical_gradient_and_zero_near_the_border() {
        let v = ramp();
        let p = v.grid().index_to_point([2.0, 1.5, 1.0]);
        let g = central_difference_at_point(&v, p);
        assert!((g[0] - 1.0).abs() < 1e-12);
        assert!((g[1] + 2.0).abs() < 1e-12);
        assert!((g[2] - 0.5).abs() < 1e-12);
        let inside = central_difference_at_point(&v, v.grid().index_to_point([0.2, 1.5, 1.0]));
        assert!((inside[0] - 0.7).abs() < 1e-12);
        let edge = central_difference_at_point(&v, v.grid().index_to_point([-0.2, 1.5, 1.0]));
        assert_eq!(edge[0], 0.0);
        assert!((edge[1] + 2.0).abs() < 1e-12);
    }
}
