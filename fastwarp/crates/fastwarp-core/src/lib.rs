// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Vectors and 4×4 affines (hot paths; TRAC/CORE, VAT front-end).

pub mod env;
pub mod nifti_affine;
pub mod sha256;

pub use env::env_var_os;
pub use nifti_affine::{voxel_to_world, NiftiXformFields, XformSource};

/// Homogeneous 4×4 row-major (NIfTI-style): world = A @ vox_h.
pub type Mat4 = [[f64; 4]; 4];

/// 4×4 row-major product `a * b` (same as NumPy `a @ b` for shape (4,4)).
#[inline]
pub fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0_f64; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            out[i][j] =
                a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + a[i][3] * b[3][j];
        }
    }
    out
}

/// Identity 4×4.
#[inline]
pub fn identity_mat4() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Apply 4×4 to ``[x,y,z,1]``.
#[inline]
pub fn apply_rowmajor(a: &Mat4, p: [f64; 3]) -> [f64; 3] {
    let w = 1.0;
    let x = a[0][0] * p[0] + a[0][1] * p[1] + a[0][2] * p[2] + a[0][3] * w;
    let y = a[1][0] * p[0] + a[1][1] * p[1] + a[1][2] * p[2] + a[1][3] * w;
    let z = a[2][0] * p[0] + a[2][1] * p[1] + a[2][2] * p[2] + a[2][3] * w;
    [x, y, z]
}

/// 3-vector dot.
#[inline]
pub fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(tx: f64, ty: f64, tz: f64) -> Mat4 {
        [
            [1.0, 0.0, 0.0, tx],
            [0.0, 1.0, 0.0, ty],
            [0.0, 0.0, 1.0, tz],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    #[test]
    fn identity() {
        let i: Mat4 = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let p = apply_rowmajor(&i, [1.0, 2.0, 3.0]);
        assert!((p[0] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn identity_mul_matches_mat4() {
        let i = identity_mat4();
        let a = tr(3.0, -2.0, 7.0);
        let l = mat4_mul(&i, &a);
        let r = mat4_mul(&a, &i);
        for ridx in 0..4 {
            for c in 0..4 {
                assert!((l[ridx][c] - a[ridx][c]).abs() < 1e-12);
                assert!((r[ridx][c] - a[ridx][c]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn mat4_mul_translation_hand_check() {
        let t1 = tr(1.0, 2.0, 3.0);
        let t2 = tr(4.0, 5.0, 6.0);
        let p = mat4_mul(&t1, &t2);
        let expect = tr(5.0, 7.0, 9.0);
        for ridx in 0..4 {
            for c in 0..4 {
                assert!((p[ridx][c] - expect[ridx][c]).abs() < 1e-12);
            }
        }
    }
}

/// Invert a 4×4 row-major affine (NIfTI index→world), or `None` when the 3×3 block is singular.
///
/// NIfTI affines are affine, not projective, so only the 3×3 direction/scale block needs
/// inverting; the translation follows as `-R⁻¹ t`.
pub fn invert_rowmajor(m: &Mat4) -> Option<Mat4> {
    let a = [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let id = 1.0 / det;
    let inv3 = [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * id,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * id,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * id,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * id,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * id,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * id,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * id,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * id,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * id,
        ],
    ];
    let t = [m[0][3], m[1][3], m[2][3]];
    let mut out = [[0.0f64; 4]; 4];
    for r in 0..3 {
        out[r][..3].copy_from_slice(&inv3[r]);
        out[r][3] = -(inv3[r][0] * t[0] + inv3[r][1] * t[1] + inv3[r][2] * t[2]);
    }
    out[3][3] = 1.0;
    Some(out)
}

/// World mm per voxel step along each index axis, from the affine's column norms.
pub fn voxel_spacing_rowmajor(m: &Mat4) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    for (a, o) in out.iter_mut().enumerate() {
        *o = (m[0][a] * m[0][a] + m[1][a] * m[1][a] + m[2][a] * m[2][a]).sqrt();
    }
    out
}

#[cfg(test)]
mod affine_tests {
    use super::*;

    #[test]
    fn inverse_round_trips_an_oblique_affine() {
        let m: Mat4 = [
            [0.9, -0.1, 0.05, -30.0],
            [0.08, 1.1, -0.2, 12.0],
            [-0.03, 0.15, 0.7, 5.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let inv = invert_rowmajor(&m).expect("non-singular");
        for p in [[0.0, 0.0, 0.0], [10.0, -4.0, 7.5], [-3.0, 2.0, 1.0]] {
            let back = apply_rowmajor(&inv, apply_rowmajor(&m, p));
            for a in 0..3 {
                assert!((back[a] - p[a]).abs() < 1e-9, "{back:?} != {p:?}");
            }
        }
    }

    #[test]
    fn singular_affine_has_no_inverse() {
        let mut m = identity_mat4();
        m[2][2] = 0.0;
        assert!(invert_rowmajor(&m).is_none());
    }

    #[test]
    fn spacing_reads_column_norms() {
        let m: Mat4 = [
            [-0.5, 0.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 1.5, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let s = voxel_spacing_rowmajor(&m);
        assert!((s[0] - 0.5).abs() < 1e-12);
        assert!((s[1] - 1.5).abs() < 1e-12);
        assert!((s[2] - 2.0).abs() < 1e-12);
    }
}
