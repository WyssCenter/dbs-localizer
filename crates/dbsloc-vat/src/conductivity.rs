// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Conductivity model (`specs/07` §3.2).

/// Reference conductivity of an ideal contact (`specs/07` §3.2); the solver uses equipotential constraints instead.
pub const CONTACT_S_PER_M: f64 = 1e8;
/// Reference conductivity of the insulation; the solver uses zero-flux boundaries instead.
pub const INSULATION_S_PER_M: f64 = 1e-16;
/// Grey-matter mask threshold after resampling.
pub const GM_MASK_THRESHOLD: f32 = 0.5;

/// Scalar volume in world millimetres (voxel to world affine, x fastest).
#[derive(Clone, Debug)]
pub struct Volume {
    /// Voxel counts.
    pub dims: [usize; 3],
    /// Voxel index to world millimetres.
    pub affine: [[f64; 4]; 4],
    /// Voxel values.
    pub data: Vec<f32>,
}

impl Volume {
    /// Nearest-neighbour sample at a world point, `None` outside the volume.
    pub fn sample_nearest(&self, world: [f64; 3]) -> Option<f32> {
        let a = &self.affine;
        let m = [
            [a[0][0], a[0][1], a[0][2]],
            [a[1][0], a[1][1], a[1][2]],
            [a[2][0], a[2][1], a[2][2]],
        ];
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if !(det.is_finite() && det.abs() > 1e-18) {
            return None;
        }
        let inv = [
            [
                (m[1][1] * m[2][2] - m[1][2] * m[2][1]) / det,
                (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / det,
                (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / det,
            ],
            [
                (m[1][2] * m[2][0] - m[1][0] * m[2][2]) / det,
                (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / det,
                (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / det,
            ],
            [
                (m[1][0] * m[2][1] - m[1][1] * m[2][0]) / det,
                (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / det,
                (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / det,
            ],
        ];
        let d = [world[0] - a[0][3], world[1] - a[1][3], world[2] - a[2][3]];
        let mut idx = [0usize; 3];
        for r in 0..3 {
            let v = inv[r][0] * d[0] + inv[r][1] * d[1] + inv[r][2] * d[2];
            let n = v.round();
            if n < 0.0 || n >= self.dims[r] as f64 {
                return None;
            }
            idx[r] = n as usize;
        }
        self.data
            .get(idx[0] + self.dims[0] * (idx[1] + self.dims[1] * idx[2]))
            .copied()
    }
}

/// Tissue conductivity assignment.
#[derive(Clone, Debug)]
pub enum ConductivityModel {
    /// One value everywhere.
    Homogeneous {
        /// Conductivity in S/m.
        s_per_m: f64,
    },
    /// Grey matter where the mask exceeds 0.5, white matter elsewhere.
    GreyWhite {
        /// Grey-matter conductivity in S/m.
        gm_s_per_m: f64,
        /// White-matter conductivity in S/m.
        wm_s_per_m: f64,
        /// Grey-matter mask in native world space; `None` falls back to white matter.
        gm_mask: Option<Volume>,
    },
}

impl ConductivityModel {
    /// Conductivity of tissue at a world point.
    pub fn sigma_at(&self, world: [f64; 3]) -> f64 {
        match self {
            ConductivityModel::Homogeneous { s_per_m } => *s_per_m,
            ConductivityModel::GreyWhite {
                gm_s_per_m,
                wm_s_per_m,
                gm_mask,
            } => match gm_mask.as_ref().and_then(|m| m.sample_nearest(world)) {
                Some(v) if v > GM_MASK_THRESHOLD => *gm_s_per_m,
                _ => *wm_s_per_m,
            },
        }
    }

    /// True when a grey-white model has no mask and therefore behaves as homogeneous white matter.
    pub fn is_missing_mask(&self) -> bool {
        matches!(self, ConductivityModel::GreyWhite { gm_mask: None, .. })
    }
}

/// Optional shell around the lead surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct Encapsulation {
    /// Shell thickness in millimetres, 0 = off.
    pub thickness_mm: f64,
    /// Shell conductivity in S/m.
    pub s_per_m: f64,
}
