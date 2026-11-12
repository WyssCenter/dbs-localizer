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
// Portions derived from ITK `itkEuler3DTransform.hxx`
// (`ComputeJacobianWithRespectToParameters`, non-ZYX and ZYX branches),
// `itkMatrixOffsetTransformBase.hxx` (`ComputeJacobianWithRespectToParameters`,
// `ComputeJacobianWithRespectToPosition`) and `itkCompositeTransform.hxx`
// (`ComputeJacobianWithRespectToParametersCachedTemporaries`: the Jacobian of the optimised
// transform is left-multiplied by the position Jacobians of the transforms applied after it).
// See `docs/THIRD_PARTY_PORTS.md`.

//! Parameterised linear transforms with their Jacobians, for the registration metrics.

use crate::error::{invalid, RegError, Result};
use crate::linalg::{mat3_mul_vec, Mat3, Vec3};
use crate::transform::affine::AffineTransform;

/// A linear transform that an optimiser can update, with ITK's parameter layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParametricTransform {
    /// `Euler3DTransform`: parameters `[angle_x, angle_y, angle_z, tx, ty, tz]`.
    Euler3d {
        /// The six parameters (radians, mm).
        parameters: [f64; 6],
        /// The rotation centre.
        center: Vec3,
        /// `ComputeZYX`: matrix `Rz Ry Rx` instead of `Rz Rx Ry`.
        zyx: bool,
    },
    /// `AffineTransform`: parameters the matrix row-major, then the translation.
    Affine {
        /// The twelve parameters.
        parameters: [f64; 12],
        /// The matrix centre.
        center: Vec3,
    },
}

impl ParametricTransform {
    /// A rigid transform with `Rz Rx Ry` composition.
    pub fn euler3d(parameters: [f64; 6], center: Vec3) -> Self {
        Self::Euler3d {
            parameters,
            center,
            zyx: false,
        }
    }

    /// The affine transform with the parameters of `t`.
    pub fn affine(t: &AffineTransform) -> Self {
        Self::Affine {
            parameters: t.parameters(),
            center: t.center,
        }
    }

    /// Number of parameters (6 or 12).
    pub fn number_of_parameters(&self) -> usize {
        match self {
            Self::Euler3d { .. } => 6,
            Self::Affine { .. } => 12,
        }
    }

    /// The parameter vector.
    pub fn parameters(&self) -> Vec<f64> {
        match self {
            Self::Euler3d { parameters, .. } => parameters.to_vec(),
            Self::Affine { parameters, .. } => parameters.to_vec(),
        }
    }

    /// Replaces the parameters; fails when the length is wrong.
    pub fn set_parameters(&mut self, values: &[f64]) -> Result<()> {
        match self {
            Self::Euler3d { parameters, .. } if values.len() == 6 => {
                parameters.copy_from_slice(values);
                Ok(())
            }
            Self::Affine { parameters, .. } if values.len() == 12 => {
                parameters.copy_from_slice(values);
                Ok(())
            }
            _ => Err(RegError::Length {
                what: "transform parameters",
                expected: self.number_of_parameters(),
                got: values.len(),
            }),
        }
    }

    /// The same transform with its parameters moved by `step * direction`.
    pub fn stepped(&self, direction: &[f64], step: f64) -> Result<Self> {
        let mut p = self.parameters();
        if direction.len() != p.len() {
            return Err(invalid(
                "transform",
                "direction length differs from parameters",
            ));
        }
        for (v, d) in p.iter_mut().zip(direction) {
            *v += step * d;
        }
        let mut out = *self;
        out.set_parameters(&p)?;
        Ok(out)
    }

    /// The equivalent [`AffineTransform`] (`y = M (x - c) + c + t`).
    pub fn to_affine(&self) -> AffineTransform {
        match self {
            Self::Euler3d {
                parameters,
                center,
                zyx,
            } => {
                let fixed = [
                    center[0],
                    center[1],
                    center[2],
                    if *zyx { 1.0 } else { 0.0 },
                ];
                AffineTransform::from_euler3d(parameters, &fixed)
                    .unwrap_or_else(|_| AffineTransform::identity())
            }
            Self::Affine { parameters, center } => {
                AffineTransform::from_parameters(parameters, center)
                    .unwrap_or_else(|_| AffineTransform::identity())
            }
        }
    }

    /// Maps a point.
    pub fn apply(&self, p: Vec3) -> Vec3 {
        self.to_affine().apply(p)
    }

    /// ITK's `ComputeJacobianWithRespectToParameters` at `p`: one 3-vector `dT/dtheta_k` per
    /// parameter.
    pub fn jacobian(&self, p: Vec3) -> Vec<Vec3> {
        let mut cols = [[0.0; 3]; 12];
        let n = self.jacobian_ctx().columns(p, &mut cols);
        cols[..n].to_vec()
    }

    /// The point-independent part of the Jacobian (the trigonometry of the Euler angles), to
    /// evaluate many points without recomputing it.
    pub fn jacobian_ctx(&self) -> JacobianCtx {
        match self {
            Self::Euler3d {
                parameters,
                center,
                zyx,
            } => JacobianCtx::Euler {
                cx: parameters[0].cos(),
                sx: parameters[0].sin(),
                cy: parameters[1].cos(),
                sy: parameters[1].sin(),
                cz: parameters[2].cos(),
                sz: parameters[2].sin(),
                center: *center,
                zyx: *zyx,
            },
            Self::Affine { center, .. } => JacobianCtx::Affine { center: *center },
        }
    }
}

/// [`ParametricTransform::jacobian_ctx`]: what the Jacobian needs besides the point.
#[derive(Clone, Copy, Debug)]
pub enum JacobianCtx {
    /// `Euler3DTransform`.
    Euler {
        /// Cosine of the angle about x.
        cx: f64,
        /// Sine of the angle about x.
        sx: f64,
        /// Cosine of the angle about y.
        cy: f64,
        /// Sine of the angle about y.
        sy: f64,
        /// Cosine of the angle about z.
        cz: f64,
        /// Sine of the angle about z.
        sz: f64,
        /// The rotation centre.
        center: Vec3,
        /// `ComputeZYX`.
        zyx: bool,
    },
    /// `AffineTransform`.
    Affine {
        /// The matrix centre.
        center: Vec3,
    },
}

impl JacobianCtx {
    /// Writes the Jacobian columns at `p` into the front of `out` and returns their number (6 or
    /// 12); the rest of `out` is left alone.
    pub fn columns(&self, p: Vec3, out: &mut [Vec3; 12]) -> usize {
        match self {
            Self::Euler {
                cx,
                sx,
                cy,
                sy,
                cz,
                sz,
                center,
                zyx,
            } => {
                let (cx, sx, cy, sy, cz, sz) = (*cx, *sx, *cy, *sy, *cz, *sz);
                let px = p[0] - center[0];
                let py = p[1] - center[1];
                let pz = p[2] - center[2];
                for c in out.iter_mut().take(6) {
                    *c = [0.0; 3];
                }
                if *zyx {
                    out[0] = [
                        (cz * sy * cx + sz * sx) * py + (-cz * sy * sx + sz * cx) * pz,
                        (sz * sy * cx - cz * sx) * py + (-sz * sy * sx - cz * cx) * pz,
                        (cy * cx) * py + (-cy * sx) * pz,
                    ];
                    out[1] = [
                        (-cz * sy) * px + (cz * cy * sx) * py + (cz * cy * cx) * pz,
                        (-sz * sy) * px + (sz * cy * sx) * py + (sz * cy * cx) * pz,
                        (-cy) * px + (-sy * sx) * py + (-sy * cx) * pz,
                    ];
                    out[2] = [
                        (-sz * cy) * px
                            + (-sz * sy * sx - cz * cx) * py
                            + (-sz * sy * cx + cz * sx) * pz,
                        (cz * cy) * px
                            + (cz * sy * sx - sz * cx) * py
                            + (cz * sy * cx + sz * sx) * pz,
                        0.0,
                    ];
                } else {
                    out[0] = [
                        (-sz * cx * sy) * px + (sz * sx) * py + (sz * cx * cy) * pz,
                        (cz * cx * sy) * px + (-cz * sx) * py + (-cz * cx * cy) * pz,
                        (sx * sy) * px + (cx) * py + (-sx * cy) * pz,
                    ];
                    out[1] = [
                        (-cz * sy - sz * sx * cy) * px + (cz * cy - sz * sx * sy) * pz,
                        (-sz * sy + cz * sx * cy) * px + (sz * cy + cz * sx * sy) * pz,
                        (-cx * cy) * px + (-cx * sy) * pz,
                    ];
                    out[2] = [
                        (-sz * cy - cz * sx * sy) * px
                            + (-cz * cx) * py
                            + (-sz * sy + cz * sx * cy) * pz,
                        (cz * cy - sz * sx * sy) * px
                            + (-sz * cx) * py
                            + (cz * sy + sz * sx * cy) * pz,
                        0.0,
                    ];
                }
                for d in 0..3 {
                    out[3 + d][d] = 1.0;
                }
                6
            }
            Self::Affine { center } => {
                for c in out.iter_mut() {
                    *c = [0.0; 3];
                }
                for r in 0..3 {
                    for c in 0..3 {
                        out[r * 3 + c][r] = p[c] - center[c];
                    }
                    out[9 + r][r] = 1.0;
                }
                12
            }
        }
    }
}

/// The moving transform of a linear registration stage: the optimised transform applied first,
/// then `outer` (the product of all transforms added before it to the composite, e.g. the
/// centre-of-mass initialiser): `x -> outer(optimised(x))`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearMovingTransform {
    /// The transform applied last (queue order: added first).
    pub outer: AffineTransform,
    /// The transform being optimised.
    pub optimised: ParametricTransform,
}

impl LinearMovingTransform {
    /// Maps a virtual-domain point to the moving image.
    pub fn apply(&self, p: Vec3) -> Vec3 {
        self.outer.apply(self.optimised.apply(p))
    }

    /// `d apply(p) / d parameters`: each column of the optimised Jacobian left-multiplied by the
    /// matrix of `outer`.
    pub fn jacobian(&self, p: Vec3) -> Vec<Vec3> {
        let mut cols = [[0.0; 3]; 12];
        let n = self.prepared().jacobian_columns(p, &mut cols);
        cols[..n].to_vec()
    }

    /// The matrices and trigonometry of this transform, computed once for many points.
    pub fn prepared(&self) -> PreparedMoving {
        PreparedMoving {
            optimised: self.optimised.to_affine(),
            outer: self.outer,
            jacobian: self.optimised.jacobian_ctx(),
        }
    }

    /// The same transform with updated parameters of the optimised part.
    pub fn with_parameters(&self, values: &[f64]) -> Result<Self> {
        let mut out = *self;
        out.optimised.set_parameters(values)?;
        Ok(out)
    }
}

/// A [`LinearMovingTransform`] with its point-independent work done: mapping and Jacobian of a
/// point give the same bits as the transform itself.
#[derive(Clone, Copy, Debug)]
pub struct PreparedMoving {
    optimised: AffineTransform,
    outer: AffineTransform,
    jacobian: JacobianCtx,
}

impl PreparedMoving {
    /// `outer(optimised(p))`.
    pub fn apply(&self, p: Vec3) -> Vec3 {
        self.outer.apply(self.optimised.apply(p))
    }

    /// Writes `d apply(p) / d parameters` into the front of `out` and returns the number of
    /// columns.
    pub fn jacobian_columns(&self, p: Vec3, out: &mut [Vec3; 12]) -> usize {
        let m: &Mat3 = &self.outer.matrix;
        let n = self.jacobian.columns(p, out);
        for c in out.iter_mut().take(n) {
            *c = mat3_mul_vec(m, *c);
        }
        n
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::vec3_norm;

    fn fd_jacobian(t: &ParametricTransform, p: Vec3) -> Vec<Vec3> {
        let base = t.parameters();
        (0..base.len())
            .map(|k| {
                let h = 1e-6;
                let mut up = base.clone();
                up[k] += h;
                let mut dn = base.clone();
                dn[k] -= h;
                let mut a = *t;
                a.set_parameters(&up).unwrap();
                let mut b = *t;
                b.set_parameters(&dn).unwrap();
                let (ya, yb) = (a.apply(p), b.apply(p));
                [
                    (ya[0] - yb[0]) / (2.0 * h),
                    (ya[1] - yb[1]) / (2.0 * h),
                    (ya[2] - yb[2]) / (2.0 * h),
                ]
            })
            .collect()
    }

    #[test]
    fn euler_jacobian_matches_finite_differences_in_both_orders() {
        let p = [12.0, -7.0, 31.0];
        for zyx in [false, true] {
            let t = ParametricTransform::Euler3d {
                parameters: [0.21, -0.17, 0.33, 1.0, -2.0, 0.5],
                center: [3.0, 4.0, -5.0],
                zyx,
            };
            let a = t.jacobian(p);
            let b = fd_jacobian(&t, p);
            for k in 0..6 {
                assert!(
                    vec3_norm([a[k][0] - b[k][0], a[k][1] - b[k][1], a[k][2] - b[k][2]]) < 1e-6,
                    "zyx {zyx} param {k}: {:?} vs {:?}",
                    a[k],
                    b[k]
                );
            }
        }
    }

    #[test]
    fn affine_jacobian_matches_finite_differences() {
        let t = ParametricTransform::Affine {
            parameters: [
                1.01, 0.02, -0.03, 0.04, 0.98, 0.01, -0.02, 0.03, 1.02, 1.0, 2.0, 3.0,
            ],
            center: [5.0, -6.0, 7.0],
        };
        let p = [12.0, -7.0, 31.0];
        let a = t.jacobian(p);
        let b = fd_jacobian(&t, p);
        for k in 0..12 {
            assert!(
                vec3_norm([a[k][0] - b[k][0], a[k][1] - b[k][1], a[k][2] - b[k][2]]) < 1e-6,
                "param {k}"
            );
        }
    }

    #[test]
    fn composite_jacobian_is_left_multiplied_by_the_outer_matrix() {
        let outer = AffineTransform::new(
            [[0.9, 0.1, 0.0], [-0.1, 0.9, 0.05], [0.0, -0.05, 1.1]],
            [4.0, 5.0, 6.0],
            [1.0, 2.0, 3.0],
        );
        let m = LinearMovingTransform {
            outer,
            optimised: ParametricTransform::euler3d([0.1, 0.2, -0.1, 1.0, 2.0, 3.0], [0.0; 3]),
        };
        let p = [3.0, -4.0, 5.0];
        let a = m.jacobian(p);
        let base = m.optimised.parameters();
        for k in 0..6 {
            let h = 1e-6;
            let mut up = base.clone();
            up[k] += h;
            let mut dn = base.clone();
            dn[k] -= h;
            let ya = m.with_parameters(&up).unwrap().apply(p);
            let yb = m.with_parameters(&dn).unwrap().apply(p);
            let fd = [
                (ya[0] - yb[0]) / (2.0 * h),
                (ya[1] - yb[1]) / (2.0 * h),
                (ya[2] - yb[2]) / (2.0 * h),
            ];
            assert!(vec3_norm([a[k][0] - fd[0], a[k][1] - fd[1], a[k][2] - fd[2]]) < 1e-6);
        }
        let mut bad = m.optimised;
        assert!(bad.set_parameters(&[0.0; 5]).is_err());
    }
}
