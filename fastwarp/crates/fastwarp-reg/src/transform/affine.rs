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
// Portions derived from ITK `itkMatrixOffsetTransformBase.hxx` (offset, centre and inverse
// semantics), `itkAffineTransform.hxx` and `itkEuler3DTransform.hxx` (`ComputeMatrix`,
// parameter and fixed-parameter layout). See `docs/THIRD_PARTY_PORTS.md`.

//! Affine transforms with ITK `MatrixOffsetTransformBase` semantics.

use crate::error::{fmt_err, RegError, Result};
use crate::linalg::{
    mat3_inverse, mat3_mul, mat3_mul_vec, vec3_add, vec3_sub, Mat3, Vec3, IDENTITY3,
};

/// `y = M (x - c) + c + t` in LPS millimetres: matrix `M`, translation `t`, centre `c`.
///
/// This is ITK's `MatrixOffsetTransformBase` (and so `AffineTransform`, `Rigid3DTransform`,
/// `Euler3DTransform` once their matrix is evaluated). It maps points of the transform's
/// output-space (the reference or fixed side) to its input-space (the moving side).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineTransform {
    /// Row-major 3x3 matrix `M`.
    pub matrix: Mat3,
    /// Translation `t` applied after the rotation about the centre.
    pub translation: Vec3,
    /// Centre `c` of the matrix.
    pub center: Vec3,
}

impl AffineTransform {
    /// The identity.
    pub fn identity() -> Self {
        Self {
            matrix: IDENTITY3,
            translation: [0.0; 3],
            center: [0.0; 3],
        }
    }

    /// Builds a transform from matrix, translation and centre.
    pub fn new(matrix: Mat3, translation: Vec3, center: Vec3) -> Self {
        Self {
            matrix,
            translation,
            center,
        }
    }

    /// `y = M x + offset` with a zero centre.
    pub fn from_offset(matrix: Mat3, offset: Vec3) -> Self {
        Self::new(matrix, offset, [0.0; 3])
    }

    /// `y = x + t`.
    pub fn from_translation(translation: Vec3) -> Self {
        Self::new(IDENTITY3, translation, [0.0; 3])
    }

    /// ITK's offset `t + c - M c`, so that `y = M x + offset`.
    pub fn offset(&self) -> Vec3 {
        let mc = mat3_mul_vec(&self.matrix, self.center);
        vec3_sub(vec3_add(self.translation, self.center), mc)
    }

    /// Maps a point.
    pub fn apply(&self, p: Vec3) -> Vec3 {
        let rotated = mat3_mul_vec(&self.matrix, vec3_sub(p, self.center));
        vec3_add(vec3_add(rotated, self.center), self.translation)
    }

    /// The inverse, with the same centre and translation `-M^-1 t` (ITK `GetInverse`).
    pub fn inverse(&self) -> Result<Self> {
        let inv = mat3_inverse(&self.matrix)
            .ok_or_else(|| RegError::NotInvertible("singular affine matrix".into()))?;
        let t = mat3_mul_vec(&inv, self.translation);
        Ok(Self::new(inv, [-t[0], -t[1], -t[2]], self.center))
    }

    /// `outer(inner(p))` as one transform with a zero centre.
    pub fn compose(outer: &Self, inner: &Self) -> Self {
        let matrix = mat3_mul(&outer.matrix, &inner.matrix);
        let offset = vec3_add(mat3_mul_vec(&outer.matrix, inner.offset()), outer.offset());
        Self::from_offset(matrix, offset)
    }

    /// The 12 ITK parameters: the matrix row-major, then the translation.
    pub fn parameters(&self) -> [f64; 12] {
        let mut out = [0.0; 12];
        for r in 0..3 {
            for c in 0..3 {
                out[r * 3 + c] = self.matrix[r][c];
            }
            out[9 + r] = self.translation[r];
        }
        out
    }

    /// The 3 ITK fixed parameters: the centre.
    pub fn fixed_parameters(&self) -> [f64; 3] {
        self.center
    }

    /// Inverse of [`AffineTransform::parameters`] and [`AffineTransform::fixed_parameters`].
    pub fn from_parameters(parameters: &[f64], fixed: &[f64]) -> Result<Self> {
        if parameters.len() != 12 {
            return Err(RegError::Length {
                what: "affine parameters",
                expected: 12,
                got: parameters.len(),
            });
        }
        if fixed.len() != 3 {
            return Err(RegError::Length {
                what: "affine fixed parameters",
                expected: 3,
                got: fixed.len(),
            });
        }
        let mut matrix = [[0.0; 3]; 3];
        for r in 0..3 {
            for c in 0..3 {
                matrix[r][c] = parameters[r * 3 + c];
            }
        }
        Ok(Self::new(
            matrix,
            [parameters[9], parameters[10], parameters[11]],
            [fixed[0], fixed[1], fixed[2]],
        ))
    }

    /// An `Euler3DTransform`: parameters `[angle_x, angle_y, angle_z, tx, ty, tz]` (radians, mm),
    /// fixed parameters `[cx, cy, cz]` plus an optional ZYX-order flag. The matrix is
    /// `Rz Rx Ry`, or `Rz Ry Rx` when the flag is non-zero.
    pub fn from_euler3d(parameters: &[f64], fixed: &[f64]) -> Result<Self> {
        if parameters.len() != 6 {
            return Err(RegError::Length {
                what: "Euler3D parameters",
                expected: 6,
                got: parameters.len(),
            });
        }
        if fixed.len() != 3 && fixed.len() != 4 {
            return Err(fmt_err(
                "ITK transform",
                format!(
                    "Euler3D fixed parameters must have 3 or 4 values, got {}",
                    fixed.len()
                ),
            ));
        }
        let (cx, sx) = (parameters[0].cos(), parameters[0].sin());
        let (cy, sy) = (parameters[1].cos(), parameters[1].sin());
        let (cz, sz) = (parameters[2].cos(), parameters[2].sin());
        let rx: Mat3 = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
        let ry: Mat3 = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
        let rz: Mat3 = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
        let zyx = fixed.get(3).is_some_and(|f| *f != 0.0);
        let matrix = if zyx {
            mat3_mul(&mat3_mul(&rz, &ry), &rx)
        } else {
            mat3_mul(&mat3_mul(&rz, &rx), &ry)
        };
        Ok(Self::new(
            matrix,
            [parameters[3], parameters[4], parameters[5]],
            [fixed[0], fixed[1], fixed[2]],
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::vec3_norm;

    fn sample() -> AffineTransform {
        AffineTransform::new(
            [
                [1.01, 0.05, -0.02],
                [-0.04, 0.97, 0.03],
                [0.01, -0.02, 1.02],
            ],
            [1.5, -2.0, 0.5],
            [10.0, -20.0, 5.0],
        )
    }

    #[test]
    fn apply_follows_the_centre_formula_and_offset_agrees() {
        let t = sample();
        let p = [3.0, 4.0, -5.0];
        let y = t.apply(p);
        let off = t.offset();
        let direct = vec3_add(mat3_mul_vec(&t.matrix, p), off);
        assert!(vec3_norm(vec3_sub(y, direct)) < 1e-12);
        assert_eq!(t.apply(t.center), vec3_add(t.center, t.translation));
    }

    #[test]
    fn inverse_and_composition_round_trip() {
        let t = sample();
        let inv = t.inverse().unwrap();
        let p = [-7.0, 2.5, 11.0];
        assert!(vec3_norm(vec3_sub(inv.apply(t.apply(p)), p)) < 1e-12);
        assert!(vec3_norm(vec3_sub(t.apply(inv.apply(p)), p)) < 1e-12);
        let c = AffineTransform::compose(&t, &inv);
        assert!(vec3_norm(vec3_sub(c.apply(p), p)) < 1e-12);
        let s = AffineTransform::from_euler3d(&[0.1, -0.2, 0.3, 1.0, 2.0, 3.0], &[1.0, 2.0, 3.0])
            .unwrap();
        let both = AffineTransform::compose(&t, &s);
        assert!(vec3_norm(vec3_sub(both.apply(p), t.apply(s.apply(p)))) < 1e-12);
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let t = AffineTransform::from_offset(
            [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
        );
        assert!(matches!(t.inverse(), Err(RegError::NotInvertible(_))));
    }

    #[test]
    fn parameters_round_trip() {
        let t = sample();
        let back =
            AffineTransform::from_parameters(&t.parameters(), &t.fixed_parameters()).unwrap();
        assert_eq!(back, t);
        assert!(AffineTransform::from_parameters(&[0.0; 11], &[0.0; 3]).is_err());
    }

    #[test]
    fn euler_rotation_about_z_and_order_flag() {
        let a = std::f64::consts::FRAC_PI_2;
        let t = AffineTransform::from_euler3d(&[0.0, 0.0, a, 0.0, 0.0, 0.0], &[0.0; 3]).unwrap();
        let y = t.apply([1.0, 0.0, 0.0]);
        assert!((y[0]).abs() < 1e-12 && (y[1] - 1.0).abs() < 1e-12);
        let p = [0.3, 0.4, 0.7, 0.0, 0.0, 0.0];
        let xyz = AffineTransform::from_euler3d(&p, &[0.0, 0.0, 0.0, 0.0]).unwrap();
        let zyx = AffineTransform::from_euler3d(&p, &[0.0, 0.0, 0.0, 1.0]).unwrap();
        assert_ne!(xyz.matrix, zyx.matrix);
        assert!(AffineTransform::from_euler3d(&p[..5], &[0.0; 3]).is_err());
    }
}
