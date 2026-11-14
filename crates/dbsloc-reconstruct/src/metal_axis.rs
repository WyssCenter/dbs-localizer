// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use nalgebra::{Matrix3, SymmetricEigen};
use ndarray::Array3;
use rayon::prelude::*;

use crate::electrode_catalog::ElectrodeGeometry;
use crate::math::{norm3, scale3, sub3};
use crate::sampling::axis_and_disc_samples_in_bounds;
use crate::trac::apex_to_proximal_metal_mm;
use fastwarp_core::Mat4;

pub const LEAD_BODY_HU: f64 = 1800.0;
pub const STEEP_CONTINUITY_MM: f64 = 30.0;
pub const MIN_STEEP_CONTINUITY: f64 = 0.9;
pub const CORE_MARGIN_BELOW_MM: f64 = 4.0;
pub const CORE_MARGIN_ABOVE_MM: f64 = 10.0;
pub const RANSAC_MIN_POINTS: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MetalAxis {
    pub deep: [f64; 3],
    pub u: [f64; 3],
    pub n_voxels: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RansacParams {
    pub inlier_mm: f64,
    pub min_length_mm: f64,
    pub n_iter: usize,
    pub seed: u64,
}

impl Default for RansacParams {
    fn default() -> Self {
        RansacParams {
            inlier_mm: 1.5,
            min_length_mm: 10.0,
            n_iter: 400,
            seed: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefineParams {
    pub threshold: f64,
    pub radii_mm: Vec<f64>,
    pub span_mm: (f64, f64),
}

impl Default for RefineParams {
    fn default() -> Self {
        RefineParams {
            threshold: LEAD_BODY_HU,
            radii_mm: vec![3.0, 2.0, 1.5],
            span_mm: (-10.0, 30.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreWindowError {
    NegativeMargin,
}

impl std::fmt::Display for CoreWindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreWindowError::NegativeMargin => {
                f.write_str("core_window margins must be non-negative")
            }
        }
    }
}

impl std::error::Error for CoreWindowError {}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        ((u128::from(self.next_u64()) * n as u128) >> 64) as usize
    }
}

pub fn metal_voxels(data: &Array3<f64>, aff: &Mat4, threshold: f64) -> (Vec<[f64; 3]>, Vec<f64>) {
    let (ni, nj, nk) = data.dim();
    let per_i: Vec<(Vec<[f64; 3]>, Vec<f64>)> = (0..ni)
        .into_par_iter()
        .map(|i| {
            let mut pts = Vec::new();
            let mut vals = Vec::new();
            for j in 0..nj {
                for k in 0..nk {
                    let v = data[[i, j, k]];
                    if v >= threshold {
                        pts.push(fastwarp_core::apply_rowmajor(
                            aff,
                            [i as f64, j as f64, k as f64],
                        ));
                        vals.push(v);
                    }
                }
            }
            (pts, vals)
        })
        .collect();
    let n: usize = per_i.iter().map(|(p, _)| p.len()).sum();
    let mut pts = Vec::with_capacity(n);
    let mut vals = Vec::with_capacity(n);
    for (p, v) in per_i {
        pts.extend(p);
        vals.extend(v);
    }
    (pts, vals)
}

fn perp_distance(p: [f64; 3], origin: [f64; 3], u: [f64; 3]) -> (f64, f64) {
    let d = sub3(p, origin);
    let t = d[0] * u[0] + d[1] * u[1] + d[2] * u[2];
    (
        t,
        norm3([d[0] - t * u[0], d[1] - t * u[1], d[2] - t * u[2]]),
    )
}

fn principal_axis(cov: Matrix3<f64>) -> [f64; 3] {
    let eig = SymmetricEigen::new(cov);
    let mut best = 0usize;
    for i in 1..3 {
        if eig.eigenvalues[i] > eig.eigenvalues[best] {
            best = i;
        }
    }
    let v = eig.eigenvectors.column(best);
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / n, v[1] / n, v[2] / n]
}

pub fn ransac_line(points: &[[f64; 3]], params: &RansacParams) -> Option<MetalAxis> {
    let n = points.len();
    if n < RANSAC_MIN_POINTS {
        return None;
    }
    let mut rng = SplitMix64(params.seed);
    let mut best: Option<(usize, [f64; 3])> = None;
    let mut best_n = 0usize;
    for _ in 0..params.n_iter {
        let i = rng.below(n);
        let mut j = rng.below(n - 1);
        if j >= i {
            j += 1;
        }
        let v = sub3(points[j], points[i]);
        let len = norm3(v);
        if len < params.min_length_mm {
            continue;
        }
        let u = scale3(v, 1.0 / len);
        let k = points
            .iter()
            .filter(|p| perp_distance(**p, points[i], u).1 < params.inlier_mm)
            .count();
        if k > best_n {
            best_n = k;
            best = Some((i, u));
        }
    }
    let (bi, bu) = best?;
    let q: Vec<[f64; 3]> = points
        .iter()
        .filter(|p| perp_distance(**p, points[bi], bu).1 < params.inlier_mm)
        .copied()
        .collect();
    let m = q.len() as f64;
    let mut c = [0.0f64; 3];
    for p in &q {
        for a in 0..3 {
            c[a] += p[a];
        }
    }
    c = scale3(c, 1.0 / m);
    let mut cov = Matrix3::<f64>::zeros();
    for p in &q {
        let d = sub3(*p, c);
        for r in 0..3 {
            for s in 0..3 {
                cov[(r, s)] += d[r] * d[s];
            }
        }
    }
    let mut u = principal_axis(cov);
    if u[2] < 0.0 {
        u = scale3(u, -1.0);
    }
    let t_min = q
        .iter()
        .map(|p| perp_distance(*p, c, u).0)
        .fold(f64::INFINITY, f64::min);
    Some(MetalAxis {
        deep: [
            c[0] + u[0] * t_min,
            c[1] + u[1] * t_min,
            c[2] + u[2] * t_min,
        ],
        u,
        n_voxels: best_n,
    })
}

pub fn refine_metal_axis(
    points: &[[f64; 3]],
    values: &[f64],
    start: [f64; 3],
    u: [f64; 3],
    params: &RefineParams,
) -> Option<MetalAxis> {
    let un = norm3(u);
    if un <= 0.0 || points.len() != values.len() {
        return None;
    }
    let mut uu = scale3(u, 1.0 / un);
    let mut p0 = start;
    let mut n_sel = 0usize;
    for &r in &params.radii_mm {
        let sel: Vec<usize> = points
            .iter()
            .enumerate()
            .filter_map(|(idx, p)| {
                let (t, perp) = perp_distance(*p, p0, uu);
                (perp < r && t > params.span_mm.0 && t < params.span_mm.1).then_some(idx)
            })
            .collect();
        n_sel = sel.len();
        if n_sel < 10 {
            return None;
        }
        let w: Vec<f64> = sel
            .iter()
            .map(|&idx| (values[idx] - params.threshold + 1.0).max(1e-6))
            .collect();
        let w_sum: f64 = w.iter().sum();
        let mut c = [0.0f64; 3];
        for (&idx, &wi) in sel.iter().zip(w.iter()) {
            for a in 0..3 {
                c[a] += points[idx][a] * wi;
            }
        }
        c = scale3(c, 1.0 / w_sum);
        let mut cov = Matrix3::<f64>::zeros();
        for (&idx, &wi) in sel.iter().zip(w.iter()) {
            let d = sub3(points[idx], c);
            for a in 0..3 {
                for b in 0..3 {
                    cov[(a, b)] += d[a] * wi * d[b];
                }
            }
        }
        cov /= w_sum;
        let mut v = principal_axis(cov);
        if v[0] * uu[0] + v[1] * uu[1] + v[2] * uu[2] < 0.0 {
            v = scale3(v, -1.0);
        }
        let t_min = sel
            .iter()
            .map(|&idx| perp_distance(points[idx], c, v).0)
            .fold(f64::INFINITY, f64::min);
        p0 = [
            c[0] + v[0] * t_min,
            c[1] + v[1] * t_min,
            c[2] + v[2] * t_min,
        ];
        uu = v;
    }
    Some(MetalAxis {
        deep: p0,
        u: uu,
        n_voxels: n_sel,
    })
}

pub fn metal_continuity(
    data: &Array3<f64>,
    aff: &Mat4,
    head: [f64; 3],
    u: [f64; 3],
    length_mm: f64,
    threshold: f64,
) -> f64 {
    let v = scale3(u, 1.0 / norm3(u));
    let tail = [
        head[0] + length_mm * v[0],
        head[1] + length_mm * v[1],
        head[2] + length_mm * v[2],
    ];
    let (_ax, disc) = axis_and_disc_samples_in_bounds(data, aff, head, tail, 0.5, 1.5);
    if disc.is_empty() {
        return 0.0;
    }
    disc.iter().filter(|d| **d >= threshold).count() as f64 / disc.len() as f64
}

pub fn core_window(
    axis: &MetalAxis,
    electrode: &ElectrodeGeometry,
    below_mm: f64,
    above_mm: f64,
) -> Result<([f64; 3], [f64; 3]), CoreWindowError> {
    if below_mm < 0.0 || above_mm < 0.0 {
        return Err(CoreWindowError::NegativeMargin);
    }
    let u = scale3(axis.u, 1.0 / norm3(axis.u));
    let reach = apex_to_proximal_metal_mm(electrode) + above_mm;
    Ok((
        [
            axis.deep[0] - below_mm * u[0],
            axis.deep[1] - below_mm * u[1],
            axis.deep[2] - below_mm * u[2],
        ],
        [
            axis.deep[0] + reach * u[0],
            axis.deep[1] + reach * u[1],
            axis.deep[2] + reach * u[2],
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::electrode_catalog::by_name;
    use crate::trac::{core_on_line, TracConfig};

    const TIP: [f64; 3] = [-12.0, -15.0, -10.0];

    fn steep_unit(tilt_deg: f64) -> [f64; 3] {
        let t = tilt_deg.to_radians();
        let mut u = [-0.25, t.sin(), t.cos()];
        u[1] = (1.0 - u[0] * u[0] - u[2] * u[2]).max(0.0).sqrt();
        scale3(u, 1.0 / norm3(u))
    }

    fn ct_with_lead(u: [f64; 3], length_mm: f64) -> (Array3<f64>, Mat4) {
        let off = [-45.0, -60.0, -25.0];
        let aff: Mat4 = [
            [1.0, 0.0, 0.0, off[0]],
            [0.0, 1.0, 0.0, off[1]],
            [0.0, 0.0, 1.0, off[2]],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let vol = Array3::from_shape_fn((90, 110, 90), |(i, j, k)| {
            let w = [i as f64 + off[0], j as f64 + off[1], k as f64 + off[2]];
            let (t, perp) = perp_distance(w, TIP, u);
            if perp < 1.0 && t >= 0.0 && t <= length_mm {
                2800.0
            } else if (80..84).contains(&k) {
                1500.0
            } else {
                30.0
            }
        });
        (vol, aff)
    }

    fn angle_deg(a: [f64; 3], b: [f64; 3]) -> f64 {
        (a[0] * b[0] + a[1] * b[1] + a[2] * b[2])
            .abs()
            .min(1.0)
            .acos()
            .to_degrees()
    }

    #[test]
    fn ransac_and_refine_recover_a_steep_lead() {
        let u = steep_unit(65.0);
        let (vol, aff) = ct_with_lead(u, 45.0);
        let (pts, vals) = metal_voxels(&vol, &aff, LEAD_BODY_HU);
        let seed = ransac_line(&pts, &RansacParams::default()).expect("seed");
        let refined = refine_metal_axis(&pts, &vals, seed.deep, seed.u, &RefineParams::default())
            .expect("refined");
        assert!(angle_deg(refined.u, u) < 2.0);
        assert!(norm3(sub3(refined.deep, TIP)) < 1.5);
    }

    #[test]
    fn ransac_is_deterministic_for_a_seed() {
        let (vol, aff) = ct_with_lead(steep_unit(50.0), 40.0);
        let (pts, _) = metal_voxels(&vol, &aff, LEAD_BODY_HU);
        let a = ransac_line(&pts, &RansacParams::default());
        let b = ransac_line(&pts, &RansacParams::default());
        assert_eq!(a, b);
    }

    fn ransac_line_reference(points: &[[f64; 3]], params: &RansacParams) -> Option<Vec<bool>> {
        let n = points.len();
        let mut rng = SplitMix64(params.seed);
        let mut best: Option<Vec<bool>> = None;
        let mut best_n = 0usize;
        for _ in 0..params.n_iter {
            let i = rng.below(n);
            let mut j = rng.below(n - 1);
            if j >= i {
                j += 1;
            }
            let v = sub3(points[j], points[i]);
            let len = norm3(v);
            if len < params.min_length_mm {
                continue;
            }
            let u = scale3(v, 1.0 / len);
            let inl: Vec<bool> = points
                .iter()
                .map(|p| perp_distance(*p, points[i], u).1 < params.inlier_mm)
                .collect();
            let k = inl.iter().filter(|b| **b).count();
            if k > best_n {
                best_n = k;
                best = Some(inl);
            }
        }
        best
    }

    fn metal_voxels_reference(
        data: &Array3<f64>,
        aff: &Mat4,
        threshold: f64,
    ) -> (Vec<[f64; 3]>, Vec<f64>) {
        let (ni, nj, nk) = data.dim();
        let mut pts = Vec::new();
        let mut vals = Vec::new();
        for i in 0..ni {
            for j in 0..nj {
                for k in 0..nk {
                    let v = data[[i, j, k]];
                    if v >= threshold {
                        pts.push(fastwarp_core::apply_rowmajor(
                            aff,
                            [i as f64, j as f64, k as f64],
                        ));
                        vals.push(v);
                    }
                }
            }
        }
        (pts, vals)
    }

    #[test]
    fn ransac_and_metal_voxels_match_the_serial_reference() {
        let (vol, aff) = ct_with_lead(steep_unit(55.0), 42.0);
        let (pts, vals) = metal_voxels(&vol, &aff, LEAD_BODY_HU);
        let (rp, rv) = metal_voxels_reference(&vol, &aff, LEAD_BODY_HU);
        assert_eq!(pts, rp);
        assert_eq!(vals, rv);
        let mut noisy = pts.clone();
        let mut g = SplitMix64(7);
        for _ in 0..200 {
            noisy.push([
                g.below(80) as f64 - 40.0,
                g.below(100) as f64 - 50.0,
                g.below(60) as f64 - 20.0,
            ]);
        }
        for seed in [0u64, 3, 11] {
            let params = RansacParams {
                seed,
                ..RansacParams::default()
            };
            let got = ransac_line(&noisy, &params).expect("line");
            let mask = ransac_line_reference(&noisy, &params).expect("reference");
            assert_eq!(got.n_voxels, mask.iter().filter(|b| **b).count());
            let q: Vec<[f64; 3]> = noisy
                .iter()
                .zip(mask.iter())
                .filter(|(_, b)| **b)
                .map(|(p, _)| *p)
                .collect();
            let m = q.len() as f64;
            let mut c = [0.0f64; 3];
            for p in &q {
                for a in 0..3 {
                    c[a] += p[a];
                }
            }
            c = scale3(c, 1.0 / m);
            let t_min = q
                .iter()
                .map(|p| perp_distance(*p, c, got.u).0)
                .fold(f64::INFINITY, f64::min);
            let deep = [
                c[0] + got.u[0] * t_min,
                c[1] + got.u[1] * t_min,
                c[2] + got.u[2] * t_min,
            ];
            assert_eq!(got.deep, deep);
        }
    }

    #[test]
    fn continuity_treats_samples_outside_the_volume_as_not_metal() {
        let (vol, aff) = ct_with_lead([0.0, 0.0, 1.0], 200.0);
        let start = [TIP[0], TIP[1], 50.0];
        let c = metal_continuity(&vol, &aff, start, [0.0, 0.0, 1.0], 30.0, LEAD_BODY_HU);
        assert!(c < 0.5, "continuity {c} counts replicated edge voxels");
    }

    #[test]
    fn bone_alone_is_not_metal() {
        let (vol, aff) = ct_with_lead(steep_unit(40.0), -1.0);
        let (pts, _) = metal_voxels(&vol, &aff, LEAD_BODY_HU);
        assert!(pts.len() < RANSAC_MIN_POINTS);
        assert!(ransac_line(&pts, &RansacParams::default()).is_none());
    }

    #[test]
    fn continuity_separates_lead_from_soft_tissue() {
        let u = steep_unit(65.0);
        let (vol, aff) = ct_with_lead(u, 45.0);
        let on = [TIP[0] + u[0], TIP[1] + u[1], TIP[2] + u[2]];
        assert!(
            metal_continuity(&vol, &aff, on, u, STEEP_CONTINUITY_MM, LEAD_BODY_HU)
                >= MIN_STEEP_CONTINUITY
        );
        let beside = [TIP[0] + 6.0, TIP[1], TIP[2]];
        assert_eq!(
            metal_continuity(&vol, &aff, beside, u, STEEP_CONTINUITY_MM, LEAD_BODY_HU),
            0.0
        );
    }

    #[test]
    fn refinement_needs_ten_points_in_the_cylinder() {
        let pts = vec![[0.0, 0.0, 0.0]; 5];
        let vals = vec![2000.0; 5];
        assert!(refine_metal_axis(
            &pts,
            &vals,
            [0.0; 3],
            [0.0, 0.0, 1.0],
            &RefineParams::default()
        )
        .is_none());
    }

    #[test]
    fn core_window_rejects_negative_margins() {
        let e = by_name("Medtronic 3387").expect("catalogued");
        let ax = MetalAxis {
            deep: [0.0; 3],
            u: [0.0, 0.0, 1.0],
            n_voxels: 50,
        };
        assert_eq!(
            core_window(&ax, e, -1.0, 10.0),
            Err(CoreWindowError::NegativeMargin)
        );
        let (deep, shallow) = core_window(&ax, e, 4.0, 10.0).expect("window");
        assert!((deep[2] + 4.0).abs() < 1e-12);
        assert!((shallow[2] - (apex_to_proximal_metal_mm(e) + 10.0)).abs() < 1e-12);
    }

    fn long_lead_ct(
        e: &ElectrodeGeometry,
        tip: [f64; 3],
        u: [f64; 3],
    ) -> (Array3<f64>, Mat4, [f64; 3]) {
        let vox = 0.5;
        let off = [-30.0, -45.0, -20.0];
        let aff: Mat4 = [
            [vox, 0.0, 0.0, off[0]],
            [0.0, vox, 0.0, off[1]],
            [0.0, 0.0, vox, off[2]],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let first = e.apex_to_first_level_mm();
        let head = [
            tip[0] + first * u[0],
            tip[1] + first * u[1],
            tip[2] + first * u[2],
        ];
        let vol = Array3::from_shape_fn((120, 170, 140), |(i, j, k)| {
            let w = [
                i as f64 * vox + off[0],
                j as f64 * vox + off[1],
                k as f64 * vox + off[2],
            ];
            let (a, perp) = perp_distance(w, tip, u);
            let shaft = perp <= 0.7 && (0.0..=60.0).contains(&a);
            if !shaft {
                return 30.0;
            }
            if e.level_z_mm
                .iter()
                .any(|z| (a - (first + z)).abs() <= 0.5 * e.contact_length_mm)
            {
                2800.0
            } else {
                400.0
            }
        });
        (vol, aff, head)
    }

    #[test]
    fn core_window_covers_a_long_lead() {
        let e = by_name("Medtronic 3391").expect("catalogued");
        let reach = apex_to_proximal_metal_mm(e);
        assert!(reach > 24.0);
        let u = {
            let v = [-0.2, 0.75, 0.62];
            scale3(v, 1.0 / norm3(v))
        };
        let tip = [-10.0, -15.0, -8.0];
        let (vol, aff, head) = long_lead_ct(e, tip, u);
        let (pts, vals) = metal_voxels(&vol, &aff, LEAD_BODY_HU);
        let seed = ransac_line(&pts, &RansacParams::default()).expect("seed");
        let axis = refine_metal_axis(&pts, &vals, seed.deep, seed.u, &RefineParams::default())
            .expect("axis");
        let (deep, shallow) =
            core_window(&axis, e, CORE_MARGIN_BELOW_MM, CORE_MARGIN_ABOVE_MM).expect("window");
        assert!(norm3(sub3(shallow, axis.deep)) >= reach + CORE_MARGIN_ABOVE_MM - 1e-9);
        let neg = vol.mapv(|v| -v);
        let cfg = TracConfig {
            distal_bloom_mm: 0.0,
            ..TracConfig::default()
        };
        let r = core_on_line(&neg, &aff, deep, shallow, true, e, &cfg).expect("core");
        assert!(norm3(sub3(r.head, head)) < 0.6);
        let tail = [
            head[0] + e.fiducial_span_mm() * u[0],
            head[1] + e.fiducial_span_mm() * u[1],
            head[2] + e.fiducial_span_mm() * u[2],
        ];
        assert!(norm3(sub3(r.tail, tail)) < 0.6);
        let short = core_on_line(
            &neg,
            &aff,
            [
                axis.deep[0] - 4.0 * axis.u[0],
                axis.deep[1] - 4.0 * axis.u[1],
                axis.deep[2] - 4.0 * axis.u[2],
            ],
            [
                axis.deep[0] + 20.0 * axis.u[0],
                axis.deep[1] + 20.0 * axis.u[1],
                axis.deep[2] + 20.0 * axis.u[2],
            ],
            true,
            e,
            &cfg,
        );
        assert!(short.is_err());
    }
}
