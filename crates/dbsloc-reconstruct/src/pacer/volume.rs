// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! CT volume grid + trilinear world sampling. See `specs/10_pacer.md` §7.

use fastwarp_core::Mat4;
use ndarray::Array3;

/// Voxel value at integer indices; affine maps **voxel index** (i, j, k) to world mm
/// in NIfTI row-major style (same as `fastwarp_core::apply_rowmajor` on (i,j,k) as f64).
pub struct CtVolume {
    /// Shape `(nk, nj, ni)`; index `[k, j, i]`.
    pub data: Array3<f32>,
    pub ni: usize,
    pub nj: usize,
    pub nk: usize,
    pub affine: Mat4,
    inv_linear: Option<nalgebra::Matrix3<f64>>,
}

impl CtVolume {
    pub fn from_array(data: Array3<f32>, affine: Mat4) -> Self {
        let shape = data.shape();
        let nk = shape[0];
        let nj = shape[1];
        let ni = shape[2];
        let a = &affine;
        let inv_linear = nalgebra::Matrix3::new(
            a[0][0], a[0][1], a[0][2], a[1][0], a[1][1], a[1][2], a[2][0], a[2][1], a[2][2],
        )
        .try_inverse();
        CtVolume {
            data,
            ni,
            nj,
            nk,
            affine,
            inv_linear,
        }
    }

    /// World coordinates in mm for **voxel index center** (i+0.5, j+0.5, k+0.5) style:
    /// use continuous indices (i,j,k) in `[0, ni)×[0, nj)×[0, nk)`.
    #[inline]
    pub fn index_to_world(&self, i: f64, j: f64, k: f64) -> [f64; 3] {
        fastwarp_core::apply_rowmajor(&self.affine, [i, j, k])
    }

    /// Trilinear interpolation at fractional indices; **NaN** if outside FOV.
    pub fn trilinear(&self, i: f64, j: f64, k: f64) -> f32 {
        if i < 0.0
            || j < 0.0
            || k < 0.0
            || i > self.ni as f64 - 1.0
            || j > self.nj as f64 - 1.0
            || k > self.nk as f64 - 1.0
        {
            return f32::NAN;
        }
        let i0 = i.floor() as usize;
        let j0 = j.floor() as usize;
        let k0 = k.floor() as usize;
        let i1 = (i0 + 1).min(self.ni - 1);
        let j1 = (j0 + 1).min(self.nj - 1);
        let k1 = (k0 + 1).min(self.nk - 1);
        let di = (i - i0 as f64) as f32;
        let dj = (j - j0 as f64) as f32;
        let dk = (k - k0 as f64) as f32;
        let v000 = self.data[(k0, j0, i0)];
        let v100 = self.data[(k0, j0, i1)];
        let v010 = self.data[(k0, j1, i0)];
        let v110 = self.data[(k0, j1, i1)];
        let v001 = self.data[(k1, j0, i0)];
        let v101 = self.data[(k1, j0, i1)];
        let v011 = self.data[(k1, j1, i0)];
        let v111 = self.data[(k1, j1, i1)];
        let v00 = v000 * (1.0 - di) + v100 * di;
        let v10 = v010 * (1.0 - di) + v110 * di;
        let v01 = v001 * (1.0 - di) + v101 * di;
        let v11 = v011 * (1.0 - di) + v111 * di;
        let v0 = v00 * (1.0 - dj) + v10 * dj;
        let v1 = v01 * (1.0 - dj) + v11 * dj;
        v0 * (1.0 - dk) + v1 * dk
    }

    /// Sample CT at world point (trilinear in index space). Returns **NaN** if outside volume.
    pub fn trilinear_world(&self, p: [f64; 3]) -> f32 {
        let Some(ijk) = self.world_to_ijk(p) else {
            return f32::NAN;
        };
        self.trilinear(ijk[0], ijk[1], ijk[2])
    }

    /// World (mm) → **continuous** floating indices (i, j, k) for `trilinear` (inverse of affine 3×3).
    pub fn world_to_ijk(&self, p: [f64; 3]) -> Option<[f64; 3]> {
        let a = &self.affine;
        let m = self.inv_linear.as_ref()?;
        let b = [p[0] - a[0][3], p[1] - a[1][3], p[2] - a[2][3]];
        Some([
            m[(0, 0)] * b[0] + m[(0, 1)] * b[1] + m[(0, 2)] * b[2],
            m[(1, 0)] * b[0] + m[(1, 1)] * b[1] + m[(1, 2)] * b[2],
            m[(2, 0)] * b[0] + m[(2, 1)] * b[1] + m[(2, 2)] * b[2],
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singular_affine_has_no_inverse() {
        let mut affine = fastwarp_core::identity_mat4();
        affine[2][2] = 0.0;
        let vol = CtVolume::from_array(Array3::<f32>::zeros((2, 2, 2)), affine);
        assert!(vol.world_to_ijk([0.0, 0.0, 0.0]).is_none());
        assert!(vol.trilinear_world([0.0, 0.0, 0.0]).is_nan());
    }

    #[test]
    fn world_to_ijk_inverts_an_oblique_affine() {
        let (c, s) = (3.4f64.to_radians().cos(), 3.4f64.to_radians().sin());
        let affine: Mat4 = [
            [-1.0, 0.0, -0.023, 127.7],
            [0.001, 0.998 * c, -0.06 - s, -105.3],
            [-0.023, 0.06 + s, 0.998 * c, -124.2],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let vol = CtVolume::from_array(Array3::<f32>::zeros((176, 256, 256)), affine);
        let ijk = [131.0, 140.5, 60.25];
        let w = vol.index_to_world(ijk[0], ijk[1], ijk[2]);
        let back = vol.world_to_ijk(w).expect("invertible");
        for a in 0..3 {
            assert!(
                (back[a] - ijk[a]).abs() < 1e-9,
                "axis {a}: {} vs {}",
                back[a],
                ijk[a]
            );
        }
    }
}
