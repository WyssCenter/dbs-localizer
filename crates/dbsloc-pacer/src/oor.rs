// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: AGPL-3.0-only

//! Optimal oblique resampling — `specs/10_pacer.md` §7.

use super::poly3::{
    der_polyval3, fit_param_poly_to_skeleton, polyval3, signed_poly_arc_length3, PolyR3,
};
use super::volume::CtVolume;

const XY_RES: f64 = 0.1;
const SND_THRESH: f32 = 1500.0;
const LOOKAHEAD_MM: f64 = 2.0;
const DEG8: usize = 8;

fn grid_xy() -> Vec<(f64, f64)> {
    let mut v = vec![];
    let mut x = -1.5;
    while x <= 1.5 + 1e-6 {
        let mut y = -1.5;
        while y <= 1.5 + 1e-6 {
            v.push((x, y));
            y += XY_RES;
        }
        x += XY_RES;
    }
    v
}

fn unit(d: [f64; 3]) -> [f64; 3] {
    let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if n < 1e-12 {
        return [0.0, 0.0, 0.0];
    }
    [d[0] / n, d[1] / n, d[2] / n]
}

/// Orthonormal (e1, e2) ⊥ `d` (unit).
fn ortho_basis(d: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let d = unit(d);
    let y = [0.0_f64, 1.0, 0.0];
    let mut e1 = [
        d[1] * y[2] - d[2] * y[1],
        d[2] * y[0] - d[0] * y[2],
        d[0] * y[1] - d[1] * y[0],
    ];
    let n1 = (e1[0] * e1[0] + e1[1] * e1[1] + e1[2] * e1[2]).sqrt();
    if n1 < 1e-6 {
        let x = [1.0_f64, 0.0, 0.0];
        e1 = [
            d[1] * x[2] - d[2] * x[1],
            d[2] * x[0] - d[0] * x[2],
            d[0] * x[1] - d[1] * x[0],
        ];
    }
    let e1 = unit(e1);
    let e2 = [
        d[1] * e1[2] - d[2] * e1[1],
        d[2] * e1[0] - d[0] * e1[2],
        d[0] * e1[1] - d[1] * e1[0],
    ];
    let e2 = unit(e2);
    (e1, e2)
}

fn one_oor_pass(
    poly: &PolyR3,
    vol: &CtVolume,
    z_res_mm: f64,
) -> (Vec<[f64; 3]>, Vec<f32>, Vec<f32>) {
    let l = super::poly3::poly_arc_length3(poly, 0.0, 1.0).max(1e-3);
    let one_mm_t = 1.0 / l;
    let step_t = z_res_mm * one_mm_t;
    let la_t = (LOOKAHEAD_MM * one_mm_t).min(0.5);
    let grid = grid_xy();
    let mut t_vals = vec![];
    let mut t = -la_t;
    while t <= 1.0 + 1e-9 {
        t_vals.push(t);
        t += step_t;
    }
    let mut skel = vec![];
    let mut med = vec![];
    for &tt in &t_vals {
        let c = polyval3(poly, tt);
        let mut tan = der_polyval3(poly, tt);
        tan = unit(tan);
        let (e1, e2) = ortho_basis(tan);
        let mut wsum: f32 = 0.0;
        let mut num = [0.0f64; 3];
        let mut vals: Vec<f32> = vec![];
        for (gx, gy) in &grid {
            let q = [
                c[0] + e1[0] * gx + e2[0] * gy,
                c[1] + e1[1] * gx + e2[1] * gy,
                c[2] + e1[2] * gx + e2[2] * gy,
            ];
            let v = vol.trilinear_world(q);
            vals.push(v);
            let w = if v.is_nan() {
                0.0f32
            } else {
                (v - SND_THRESH).max(0.0)
            };
            wsum += w;
            num[0] += q[0] * w as f64;
            num[1] += q[1] * w as f64;
            num[2] += q[2] * w as f64;
        }
        if wsum < 1e-12 {
            skel.push(c);
            med.push(0.0);
            continue;
        }
        let p = [
            num[0] / wsum as f64,
            num[1] / wsum as f64,
            num[2] / wsum as f64,
        ];
        skel.push(p);
        vals.retain(|v| !v.is_nan());
        med.push(median_f32(&mut vals));
    }
    let s_mm = arc_positions(poly, &t_vals);
    (skel, med, s_mm)
}

fn median_f32(fin: &mut [f32]) -> f32 {
    if fin.is_empty() {
        return 0.0;
    }
    let mid = fin.len() / 2;
    let even = fin.len().is_multiple_of(2);
    let (left, upper, _) = fin.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    let upper = *upper;
    if even {
        let lower = left
            .iter()
            .copied()
            .max_by(|a, b| a.total_cmp(b))
            .unwrap_or(upper);
        0.5 * (lower + upper)
    } else {
        upper
    }
}

fn arc_positions(poly: &PolyR3, t_vals: &[f64]) -> Vec<f32> {
    let mut s = vec![0.0f64; t_vals.len()];
    let first_pos = t_vals.iter().position(|&t| t >= 0.0);
    let n_neg = first_pos.unwrap_or(t_vals.len());
    if let Some(p) = first_pos {
        s[p] = signed_poly_arc_length3(poly, 0.0, t_vals[p]);
        for idx in p + 1..t_vals.len() {
            s[idx] = s[idx - 1] + signed_poly_arc_length3(poly, t_vals[idx - 1], t_vals[idx]);
        }
    }
    if n_neg > 0 {
        let last = n_neg - 1;
        s[last] = -signed_poly_arc_length3(poly, t_vals[last], 0.0);
        for idx in (0..last).rev() {
            s[idx] = s[idx + 1] - signed_poly_arc_length3(poly, t_vals[idx], t_vals[idx + 1]);
        }
    }
    s.into_iter().map(|v| v as f32).collect()
}

/// OOR: first pass on `initial` → refit deg 8; second OOR for intensity profile and `s_mm` (spec §7).
pub fn run_oor(initial: &PolyR3, vol: &CtVolume) -> (PolyR3, Vec<f32>, Vec<f32>) {
    const ZR: f64 = 0.025;
    let (s2, _m0, _s0) = one_oor_pass(initial, vol, ZR);
    if s2.len() < 4 {
        return (initial.clone(), vec![], vec![]);
    }
    let p2 = fit_param_poly_to_skeleton(s2.as_slice(), DEG8.min(s2.len() - 1))
        .unwrap_or_else(|| initial.clone());
    let (_s3, med, s_mm) = one_oor_pass(&p2, vol, ZR);
    (p2, med, s_mm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nalgebra::DMatrix;
    use ndarray::Array3;

    #[test]
    fn steps_without_metal_keep_the_curve_point_and_a_zero_median() {
        let below_threshold = 1000.0f32;
        let data = Array3::from_shape_fn((40, 11, 11), |(k, j, i)| {
            if k >= 20 && i == 5 && j == 5 {
                4000.0
            } else {
                below_threshold
            }
        });
        let vol = CtVolume::from_array(data, fastwarp_core::identity_mat4());
        let coeff = DMatrix::from_row_slice(2, 3, &[0.0, 0.0, 30.0, 5.0, 5.0, 5.0]);
        let poly = PolyR3::from_coeff(1, coeff).expect("line");
        let (skel, med, s_mm) = one_oor_pass(&poly, &vol, 0.025);
        let la_t = LOOKAHEAD_MM / 30.0;
        let step_t = 0.025 / 30.0;
        let n_steps = ((1.0 + la_t) / step_t).floor() as usize + 1;
        assert!(skel.len().abs_diff(n_steps) <= 1, "{} steps", skel.len());
        assert_eq!(med.len(), skel.len());
        assert_eq!(s_mm.len(), skel.len());
        assert!(
            (s_mm[0] + LOOKAHEAD_MM as f32).abs() < 1e-3,
            "s0 = {}",
            s_mm[0]
        );
        let mut empty_steps = 0;
        for (idx, (p, m)) in skel.iter().zip(&med).enumerate() {
            let t = -la_t + idx as f64 * step_t;
            if 5.0 + 30.0 * t < 18.5 {
                empty_steps += 1;
                let c = polyval3(&poly, t);
                assert!((0..3).all(|a| (p[a] - c[a]).abs() < 1e-6), "step {idx}");
                assert_eq!(*m, 0.0, "step {idx}");
            }
        }
        assert!(empty_steps > 500, "{empty_steps} metal-free steps");
        let metal = med
            .iter()
            .zip(&s_mm)
            .filter(|(_, &s)| s > 16.0)
            .all(|(m, _)| *m > 0.0);
        assert!(metal, "steps on the metal carry the sampled median");
    }
}
