// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Image grids in ITK conventions.
//!
//! A [`Grid`] is `(size, spacing, origin, direction)` in ITK's LPS physical space (millimetres):
//! `p = origin + direction * diag(spacing) * index`. `+x` points to the patient's left, `+y` to
//! posterior, `+z` to superior. NIfTI and the rest of this repository use RAS; [`ras_to_lps`] and
//! [`lps_to_ras`] convert at the boundary.

use fastwarp_core::Mat4;

use crate::error::{RegError, Result};
use crate::linalg::{mat3_inverse, mat3_max_abs_diff, mat3_mul_vec, Mat3, Vec3};
use crate::nifti_io::MAX_DIM;

/// Flips the sign of the first two components: RAS to LPS (and back; the map is an involution).
pub fn ras_to_lps(p: Vec3) -> Vec3 {
    [-p[0], -p[1], p[2]]
}

/// Flips the sign of the first two components: LPS to RAS (and back; the map is an involution).
pub fn lps_to_ras(p: Vec3) -> Vec3 {
    [-p[0], -p[1], p[2]]
}

/// Number of values of an ITK displacement-field transform's fixed parameters: size (3), origin
/// (3), spacing (3), direction (9, row-major).
pub const GRID_FIXED_PARAMETERS: usize = 18;

/// A regular 3-D grid in ITK LPS physical space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    size: [usize; 3],
    spacing: Vec3,
    origin: Vec3,
    direction: Mat3,
    index_from_offset: Mat3,
}

impl Grid {
    /// Builds a grid. `direction` holds the axis directions as columns (`direction[row][axis]`),
    /// as in ITK. Fails for a zero size, a non-positive or non-finite spacing, a non-finite
    /// origin or direction, or a singular direction.
    pub fn new(size: [usize; 3], spacing: Vec3, origin: Vec3, direction: Mat3) -> Result<Self> {
        if size.contains(&0) {
            return Err(RegError::InvalidGrid(format!("zero size {size:?}")));
        }
        if size[0]
            .checked_mul(size[1])
            .and_then(|n| n.checked_mul(size[2]))
            .and_then(|n| n.checked_mul(3))
            .is_none()
        {
            return Err(RegError::InvalidGrid(format!(
                "voxel count of size {size:?} overflows"
            )));
        }
        if spacing.iter().any(|s| !s.is_finite() || *s <= 0.0) {
            return Err(RegError::InvalidGrid(format!(
                "spacing must be positive and finite, got {spacing:?}"
            )));
        }
        if origin.iter().any(|v| !v.is_finite())
            || direction.iter().flatten().any(|v| !v.is_finite())
        {
            return Err(RegError::InvalidGrid(
                "origin and direction must be finite".into(),
            ));
        }
        let mut scaled = direction;
        for row in &mut scaled {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell *= spacing[c];
            }
        }
        let index_from_offset = mat3_inverse(&scaled)
            .ok_or_else(|| RegError::InvalidGrid("singular direction matrix".into()))?;
        Ok(Self {
            size,
            spacing,
            origin,
            direction,
            index_from_offset,
        })
    }

    /// Voxels per axis.
    pub fn size(&self) -> [usize; 3] {
        self.size
    }

    /// Voxel spacing per axis in mm.
    pub fn spacing(&self) -> Vec3 {
        self.spacing
    }

    /// Physical position of voxel `(0, 0, 0)` in LPS mm.
    pub fn origin(&self) -> Vec3 {
        self.origin
    }

    /// Axis directions as columns (`direction[row][axis]`).
    pub fn direction(&self) -> Mat3 {
        self.direction
    }

    /// Total voxel count.
    pub fn num_voxels(&self) -> usize {
        self.size[0] * self.size[1] * self.size[2]
    }

    /// Linear offset of voxel `(i, j, k)`, x-fastest.
    pub fn linear_index(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.size[0] * (j + self.size[1] * k)
    }

    /// Inverse of [`Grid::linear_index`].
    pub fn ijk(&self, linear: usize) -> [usize; 3] {
        let nx = self.size[0];
        let ny = self.size[1];
        [linear % nx, (linear / nx) % ny, linear / (nx * ny)]
    }

    /// Physical position (LPS mm) of a continuous index.
    pub fn index_to_point(&self, index: Vec3) -> Vec3 {
        let scaled = [
            index[0] * self.spacing[0],
            index[1] * self.spacing[1],
            index[2] * self.spacing[2],
        ];
        let rotated = mat3_mul_vec(&self.direction, scaled);
        [
            self.origin[0] + rotated[0],
            self.origin[1] + rotated[1],
            self.origin[2] + rotated[2],
        ]
    }

    /// Continuous index of a physical position (LPS mm).
    pub fn point_to_index(&self, p: Vec3) -> Vec3 {
        mat3_mul_vec(
            &self.index_from_offset,
            [
                p[0] - self.origin[0],
                p[1] - self.origin[1],
                p[2] - self.origin[2],
            ],
        )
    }

    /// ITK's `IsInsideBuffer` for a continuous index: each coordinate in `[-0.5, size - 0.5)`.
    pub fn contains_continuous_index(&self, c: Vec3) -> bool {
        (0..3).all(|a| c[a] >= -0.5 && c[a] < self.size[a] as f64 - 0.5)
    }

    /// The 18 ITK fixed parameters: size, origin, spacing, direction (row-major).
    pub fn fixed_parameters(&self) -> [f64; GRID_FIXED_PARAMETERS] {
        let mut out = [0.0; GRID_FIXED_PARAMETERS];
        for a in 0..3 {
            out[a] = self.size[a] as f64;
            out[3 + a] = self.origin[a];
            out[6 + a] = self.spacing[a];
            for b in 0..3 {
                out[9 + a * 3 + b] = self.direction[a][b];
            }
        }
        out
    }

    /// Inverse of [`Grid::fixed_parameters`].
    pub fn from_fixed_parameters(p: &[f64]) -> Result<Self> {
        if p.len() != GRID_FIXED_PARAMETERS {
            return Err(RegError::Length {
                what: "displacement field fixed parameters",
                expected: GRID_FIXED_PARAMETERS,
                got: p.len(),
            });
        }
        let mut size = [0usize; 3];
        for a in 0..3 {
            let v = p[a];
            if !(v.is_finite() && v >= 1.0 && v.fract() == 0.0 && v <= MAX_DIM as f64) {
                return Err(RegError::InvalidGrid(format!("bad size parameter {v}")));
            }
            size[a] = v as usize;
        }
        let mut direction = [[0.0; 3]; 3];
        for a in 0..3 {
            for b in 0..3 {
                direction[a][b] = p[9 + a * 3 + b];
            }
        }
        Self::new(size, [p[6], p[7], p[8]], [p[3], p[4], p[5]], direction)
    }

    /// Voxel-to-RAS-world affine, the matrix NIfTI stores: columns are `direction` axes scaled
    /// by the spacing, with the x and y rows sign-flipped and the origin flipped likewise.
    pub fn ras_affine(&self) -> Mat4 {
        let mut m = [[0.0; 4]; 4];
        for (r, row) in m.iter_mut().enumerate().take(3) {
            let flip = if r < 2 { -1.0 } else { 1.0 };
            for (c, cell) in row.iter_mut().enumerate().take(3) {
                *cell = flip * self.direction[r][c] * self.spacing[c];
            }
            row[3] = flip * self.origin[r];
        }
        m[3][3] = 1.0;
        m
    }

    /// Grid of a voxel-to-RAS-world affine: spacing is the column norm, direction the
    /// normalised columns (sign-flipped back to LPS), origin the flipped translation.
    pub fn from_ras_affine(size: [usize; 3], m: &Mat4) -> Result<Self> {
        let mut spacing = [0.0; 3];
        let mut direction = [[0.0; 3]; 3];
        for c in 0..3 {
            let n = (m[0][c] * m[0][c] + m[1][c] * m[1][c] + m[2][c] * m[2][c]).sqrt();
            spacing[c] = n;
            if n > 0.0 {
                for r in 0..3 {
                    let flip = if r < 2 { -1.0 } else { 1.0 };
                    direction[r][c] = flip * m[r][c] / n;
                }
            }
        }
        Self::new(size, spacing, [-m[0][3], -m[1][3], m[2][3]], direction)
    }

    /// Whether size is equal and spacing, origin and direction agree within `tol`.
    pub fn approx_eq(&self, other: &Grid, tol: f64) -> bool {
        self.size == other.size
            && (0..3).all(|a| {
                (self.spacing[a] - other.spacing[a]).abs() <= tol
                    && (self.origin[a] - other.origin[a]).abs() <= tol
            })
            && mat3_max_abs_diff(&self.direction, &other.direction) <= tol
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    #[test]
    fn fixed_parameter_sizes_are_capped_and_voxel_counts_cannot_overflow() {
        let mut p = Grid::new([2, 3, 4], [1.0; 3], [0.0; 3], IDENTITY3)
            .unwrap()
            .fixed_parameters();
        p[0] = (crate::nifti_io::MAX_DIM + 1) as f64;
        assert!(matches!(
            Grid::from_fixed_parameters(&p),
            Err(RegError::InvalidGrid(_))
        ));
        p[0] = crate::nifti_io::MAX_DIM as f64;
        assert!(Grid::from_fixed_parameters(&p).is_ok());
        assert!(matches!(
            Grid::new([usize::MAX / 2, 3, 1], [1.0; 3], [0.0; 3], IDENTITY3),
            Err(RegError::InvalidGrid(_))
        ));
    }

    #[test]
    fn index_point_round_trip_with_oblique_direction() {
        let c = 0.3f64.cos();
        let s = 0.3f64.sin();
        let dir = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
        let g = Grid::new([10, 11, 12], [0.9, 1.1, 2.5], [-5.0, 7.0, 3.0], dir).unwrap();
        let idx = [3.25, 8.5, 1.75];
        let back = g.point_to_index(g.index_to_point(idx));
        for a in 0..3 {
            assert!((back[a] - idx[a]).abs() < 1e-12);
        }
        let p = g.index_to_point([1.0, 0.0, 0.0]);
        assert!((p[0] - (-5.0 + 0.9 * c)).abs() < 1e-12);
        assert!((p[1] - (7.0 + 0.9 * s)).abs() < 1e-12);
    }

    #[test]
    fn linear_index_is_x_fastest_and_invertible() {
        let g = Grid::new([4, 5, 6], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        assert_eq!(g.linear_index(1, 0, 0), 1);
        assert_eq!(g.linear_index(0, 1, 0), 4);
        assert_eq!(g.linear_index(0, 0, 1), 20);
        for l in [0, 7, 59, 119] {
            let [i, j, k] = g.ijk(l);
            assert_eq!(g.linear_index(i, j, k), l);
        }
    }

    #[test]
    fn buffer_test_matches_itk_half_voxel_rule() {
        let g = Grid::new([4, 4, 4], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        assert!(g.contains_continuous_index([-0.5, 0.0, 3.49]));
        assert!(!g.contains_continuous_index([-0.51, 0.0, 0.0]));
        assert!(!g.contains_continuous_index([0.0, 3.5, 0.0]));
    }

    #[test]
    fn fixed_parameters_round_trip() {
        let dir = [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let g = Grid::new([7, 8, 9], [2.0, 2.5, 3.0], [1.5, -2.5, 9.0], dir).unwrap();
        let p = g.fixed_parameters();
        assert_eq!(p[0], 7.0);
        assert_eq!(p[9 + 1], -1.0);
        assert_eq!(Grid::from_fixed_parameters(&p).unwrap(), g);
        assert!(Grid::from_fixed_parameters(&p[..17]).is_err());
    }

    #[test]
    fn ras_affine_round_trip_and_flip() {
        let g = Grid::new([5, 5, 5], [1.0, 2.0, 3.0], [10.0, 20.0, 30.0], IDENTITY3).unwrap();
        let m = g.ras_affine();
        assert_eq!(m[0][0], -1.0);
        assert_eq!(m[1][1], -2.0);
        assert_eq!(m[2][2], 3.0);
        assert_eq!([m[0][3], m[1][3], m[2][3]], [-10.0, -20.0, 30.0]);
        let back = Grid::from_ras_affine([5, 5, 5], &m).unwrap();
        assert!(back.approx_eq(&g, 1e-12));
    }

    #[test]
    fn rejects_bad_grids() {
        assert!(Grid::new([0, 1, 1], [1.0; 3], [0.0; 3], IDENTITY3).is_err());
        assert!(Grid::new([1, 1, 1], [0.0, 1.0, 1.0], [0.0; 3], IDENTITY3).is_err());
        assert!(Grid::new([1, 1, 1], [1.0; 3], [0.0; 3], [[0.0; 3]; 3]).is_err());
    }
}
