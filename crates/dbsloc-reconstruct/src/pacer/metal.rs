// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Metal blob CC + PCA filtering — `specs/10_pacer.md` §5.

use nalgebra::{Matrix3, SymmetricEigen, Vector3};
use ndarray::Array3;

use super::errors::{PacerError, PacerResult};
use super::mask::par_fill;

const LAMBDA1_MIN: f64 = 25.0;
const AXIS_RATIO: f64 = 10.0;
const FLAT_MAX: f64 = 8.0;

/// High-Z metal components inside brain, converted to world mm and HU list.
pub struct PointCloud {
    pub points_mm: Vec<[f64; 3]>,
    pub values: Vec<f32>,
}

/// `vox` = voxel size (dx, dy, dz) in mm, `affine` for index→world.
pub fn min_max_voxel_bounds(vox: [f64; 3]) -> (f64, f64) {
    let d = 1.27_f64 * 0.5;
    let s = vox[0] * vox[1] * vox[2];
    let min_v = 1.2 * d * d * std::f64::consts::PI * 40.0 / s;
    let max_v = 3.0 * d * d * std::f64::consts::PI * 80.0 / s;
    (min_v, max_v)
}

/// PCA on `points` (mm). Returns `(a1, a2, a3), centroid` with `a_i = 2*sqrt(λ_i)` in descending order.
pub(crate) fn pca_axis_lengths_mm(points: &[[f64; 3]]) -> Option<([f64; 3], [f64; 3])> {
    let n = points.len() as f64;
    if n < 3.0 {
        return None;
    }
    let c = {
        let mut s = [0.0; 3];
        for p in points {
            s[0] += p[0];
            s[1] += p[1];
            s[2] += p[2];
        }
        [s[0] / n, s[1] / n, s[2] / n]
    };
    let mut cov = Matrix3::zeros();
    for p in points {
        let d = Vector3::new(p[0] - c[0], p[1] - c[1], p[2] - c[2]);
        cov += d * d.transpose();
    }
    cov /= n;
    let se = SymmetricEigen::new(cov);
    let mut lams = [
        se.eigenvalues[0].max(0.0),
        se.eigenvalues[1].max(0.0),
        se.eigenvalues[2].max(0.0),
    ];
    lams.sort_by(f64::total_cmp);
    let lam3 = lams[0];
    let lam2 = lams[1];
    let lam1 = lams[2];
    let a1 = 2.0 * lam1.sqrt();
    let a2 = 2.0 * lam2.sqrt();
    let a3 = 2.0 * lam3.sqrt();
    Some(([a1, a2, a3], c))
}

fn is_electrode_shape(a1: f64, a2: f64, a3: f64) -> bool {
    let m23 = 0.5 * (a2 + a3);
    let eps = 1e-6;
    a1 > LAMBDA1_MIN && a1 / m23 > AXIS_RATIO && a3 / (a2 + eps) < FLAT_MAX
}

/// Exposes `is_electrode_shape` to unit tests without widening the public API.
pub(crate) fn is_electrode_shape_export(a1: f64, a2: f64, a3: f64) -> bool {
    is_electrode_shape(a1, a2, a3)
}

/// 26-connected CC labeling; returns (labels, nlabels).
fn label_cc_26(m: &Array3<bool>) -> (ndarray::Array3<i32>, i32) {
    let (nk, nj, ni) = (m.shape()[0], m.shape()[1], m.shape()[2]);
    let mut lab = ndarray::Array3::from_elem((nk, nj, ni), 0i32);
    let mut id = 0i32;
    for k in 0..nk {
        for j in 0..nj {
            for i in 0..ni {
                if !m[(k, j, i)] || lab[(k, j, i)] > 0 {
                    continue;
                }
                id += 1;
                let mut st = vec![(k, j, i)];
                lab[(k, j, i)] = id;
                while let Some((ck, cj, ci)) = st.pop() {
                    for di in -1i32..=1 {
                        for dj in -1i32..=1 {
                            for dk in -1i32..=1 {
                                if (di | dj | dk) == 0 {
                                    continue;
                                }
                                let ii = ci as i32 + di;
                                let jj = cj as i32 + dj;
                                let kk = ck as i32 + dk;
                                if ii < 0
                                    || jj < 0
                                    || kk < 0
                                    || ii as usize >= ni
                                    || jj as usize >= nj
                                    || kk as usize >= nk
                                {
                                    continue;
                                }
                                let (ii, jj, kk) = (ii as usize, jj as usize, kk as usize);
                                if m[(kk, jj, ii)] && lab[(kk, jj, ii)] == 0 {
                                    lab[(kk, jj, ii)] = id;
                                    st.push((kk, jj, ii));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (lab, id)
}

fn extract_components(
    lab: &ndarray::Array3<i32>,
    labels: &[usize],
    nlab: usize,
    vol: &Array3<f32>,
    to_world: impl Fn(f64, f64, f64) -> [f64; 3],
) -> Vec<PointCloud> {
    let mut slot = vec![usize::MAX; nlab + 1];
    for (s, &l) in labels.iter().enumerate() {
        slot[l] = s;
    }
    let mut out: Vec<PointCloud> = labels
        .iter()
        .map(|_| PointCloud {
            points_mm: vec![],
            values: vec![],
        })
        .collect();
    for ((k, j, i), &l) in lab.indexed_iter() {
        if l <= 0 {
            continue;
        }
        let s = slot[l as usize];
        if s == usize::MAX {
            continue;
        }
        out[s]
            .points_mm
            .push(to_world(i as f64, j as f64, k as f64));
        out[s].values.push(vol[(k, j, i)]);
    }
    out
}

/// `metal_hu` = threshold after +1024 rule applied to raw CT.
/// `affine` maps voxel indices (i,j,k) to world mm.
pub fn find_electrode_clouds(
    vol: &Array3<f32>,
    brain: &Array3<bool>,
    vox: [f64; 3],
    mut metal_hu: f32,
    to_world: impl Copy + Fn(f64, f64, f64) -> [f64; 3],
) -> PacerResult<Vec<PointCloud>> {
    let (min_v, max_v) = min_max_voxel_bounds(vox);
    for _ in 0..6 {
        let (nk, nj, ni) = vol.dim();
        let th = par_fill((nk, nj, ni), false, |k, out| {
            for j in 0..nj {
                for i in 0..ni {
                    out[j * ni + i] = brain[(k, j, i)] && vol[(k, j, i)] > metal_hu;
                }
            }
        });
        let (lab, nlab) = label_cc_26(&th);
        if nlab == 0 {
            if metal_hu < 3000.0 {
                metal_hu *= 1.2;
                continue;
            } else {
                return Err(PacerError::NoElectrode);
            }
        }
        let mut comp_areas = vec![0usize; (nlab + 1) as usize];
        for v in lab.iter() {
            if *v > 0 {
                comp_areas[*v as usize] += 1;
            }
        }
        let mut cands: Vec<usize> = (1..=nlab as usize)
            .filter(|&idx| {
                let a = comp_areas[idx] as f64;
                a >= min_v && a <= max_v
            })
            .collect();
        cands.sort_unstable_by_key(|&idx| std::cmp::Reverse(comp_areas[idx]));
        let mut out: Vec<PointCloud> = vec![];
        for cloud in extract_components(&lab, &cands, nlab as usize, vol, to_world) {
            if cloud.points_mm.len() < 4 {
                continue;
            }
            if let Some(([a1, a2, a3], _c)) = pca_axis_lengths_mm(&cloud.points_mm) {
                if is_electrode_shape_export(a1, a2, a3) {
                    out.push(cloud);
                }
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
        if metal_hu < 3000.0 {
            metal_hu *= 1.2;
        } else {
            return Err(PacerError::NoElectrode);
        }
    }
    Err(PacerError::NoElectrode)
}

#[cfg(test)]
mod tests {
    use super::*;

    type RodPlusXCloudParams = (Array3<f32>, Array3<bool>, fn(f64, f64, f64) -> [f64; 3]);

    #[test]
    fn cloud_points_sit_at_voxel_centres() {
        let (nk, nj, ni) = (3_usize, 3, 60);
        let vol = Array3::<f32>::from_elem((nk, nj, ni), 9000.0);
        let brain = Array3::from_elem((nk, nj, ni), true);
        let aff: fastwarp_core::Mat4 = [
            [0.8, 0.0, 0.0, -20.0],
            [0.0, 0.8, 0.0, 3.0],
            [0.0, 0.0, 0.8, 7.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let to_world = move |i: f64, j: f64, k: f64| fastwarp_core::apply_rowmajor(&aff, [i, j, k]);
        let clouds =
            find_electrode_clouds(&vol, &brain, [0.8, 0.8, 0.8], 2000.0, to_world).expect("rod");
        assert_eq!(clouds.len(), 1);
        let c = &clouds[0];
        let want: Vec<[f64; 3]> = vol
            .indexed_iter()
            .map(|((k, j, i), _)| {
                fastwarp_core::apply_rowmajor(&aff, [i as f64, j as f64, k as f64])
            })
            .collect();
        assert_eq!(c.points_mm, want);
    }

    #[test]
    fn components_are_collected_in_one_pass_in_scan_order() {
        let mut lab = ndarray::Array3::<i32>::zeros((3, 4, 5));
        let vol = Array3::from_shape_fn((3, 4, 5), |(k, j, i)| (k * 100 + j * 10 + i) as f32);
        lab[(0, 1, 2)] = 2;
        lab[(2, 3, 4)] = 2;
        lab[(1, 0, 0)] = 1;
        lab[(1, 2, 3)] = 3;
        lab[(0, 0, 4)] = 3;
        let got = extract_components(&lab, &[3, 2], 3, &vol, |i, j, k| [i, j, k]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].points_mm, vec![[4.0, 0.0, 0.0], [3.0, 2.0, 1.0]]);
        assert_eq!(got[0].values, vec![4.0, 123.0]);
        assert_eq!(got[1].points_mm, vec![[2.0, 1.0, 0.0], [4.0, 3.0, 2.0]]);
    }

    #[test]
    fn is_electrode_shape_export_long_thin_accepts() {
        assert!(is_electrode_shape_export(30.0, 2.0, 2.0));
    }

    #[test]
    fn is_electrode_shape_export_isotropic_rejects() {
        assert!(!is_electrode_shape_export(5.0, 5.0, 5.0));
    }

    fn rod_grid_to_world(i: f64, j: f64, k: f64) -> [f64; 3] {
        [
            2.0 * i,
            j + 0.01 * f64::sin(i * 0.2),
            k + 0.01 * f64::cos(i * 0.15),
        ]
    }

    fn rod_plus_x_cloud_params() -> RodPlusXCloudParams {
        let (nk, nj, ni) = (2_usize, 2, 50);
        let mut vol = Array3::zeros((nk, nj, ni));
        let mut brain = Array3::from_elem((nk, nj, ni), false);
        for k in 0..nk {
            for j in 0..nj {
                for i in 0..50 {
                    vol[(k, j, i)] = 10_000.0;
                    brain[(k, j, i)] = true;
                }
            }
        }
        (vol, brain, rod_grid_to_world)
    }

    /// Same geometry as `find_electrode_clouds_accepts_rod_plus_x`: PCA + `is_electrode_shape` must accept.
    #[test]
    fn pca_accepts_rod_plus_x_synthetic() {
        let (vol, _brain, to_world) = rod_plus_x_cloud_params();
        let (nk, nj, ni) = (2_usize, 2, 50);
        let mut pts: Vec<[f64; 3]> = Vec::new();
        for k in 0..nk {
            for j in 0..nj {
                for i in 0..ni {
                    if vol[(k, j, i)] > 0.0 {
                        pts.push(to_world(i as f64 + 0.5, j as f64 + 0.5, k as f64 + 0.5));
                    }
                }
            }
        }
        let ([a1, a2, a3], _) = pca_axis_lengths_mm(&pts).expect("pca on rod");
        assert!(
            is_electrode_shape_export(a1, a2, a3),
            "PCA axes ({a1}, {a2}, {a3}) not electrode-like"
        );
    }

    /// Synthetic high-Z rod along +X (mm), tiny Y/Z wobble. Voxel count in range from
    /// `min_max_voxel_bounds` for 1 mm isotropic voxels; asserts `find_electrode_clouds` OK.
    #[test]
    fn find_electrode_clouds_accepts_rod_plus_x() {
        let (vol, brain, to_world) = rod_plus_x_cloud_params();
        let out = find_electrode_clouds(&vol, &brain, [1.0, 1.0, 1.0], 2000.0, to_world);
        let clouds = out.expect("rod+noise should be detected as electrode");
        assert!(!clouds.is_empty());
    }
}
