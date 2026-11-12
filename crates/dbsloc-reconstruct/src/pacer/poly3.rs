// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Parametric polynomial curve in ℝ³, parameter t ∈ [0, 1].
//! Coefficient layout matches common `polyval` (high degree first).
//!
//! See `specs/10_pacer.md` §6, §8.

use nalgebra::{DMatrix, DVector};

/// Column `a` = axis x/y/z: rows `0..=degree` = coefficients for `t^degree … t^0`.
#[derive(Clone, Debug, PartialEq)]
pub struct PolyR3 {
    pub degree: usize,
    pub coeff: DMatrix<f64>,
}

impl PolyR3 {
    pub fn from_coeff(degree: usize, coeff: DMatrix<f64>) -> Option<Self> {
        if coeff.nrows() != degree + 1 || coeff.ncols() != 3 {
            return None;
        }
        Some(PolyR3 { degree, coeff })
    }
}

pub fn polyval3(poly: &PolyR3, t: f64) -> [f64; 3] {
    let col = |a: usize| {
        let c = poly.coeff.column(a);
        let mut v = c[0];
        for r in 1..=poly.degree {
            v = v * t + c[r];
        }
        v
    };
    [col(0), col(1), col(2)]
}

/// `p′(t)` in ℝ³.
pub(crate) fn der_polyval3(poly: &PolyR3, t: f64) -> [f64; 3] {
    let d = poly.degree;
    let col = |a: usize| {
        if d == 0 {
            return 0.0;
        }
        let c = poly.coeff.column(a);
        let mut v = d as f64 * c[0];
        for r in 1..d {
            v = v * t + (d - r) as f64 * c[r];
        }
        v
    };
    [col(0), col(1), col(2)]
}

fn speed(poly: &PolyR3, t: f64) -> f64 {
    let p = der_polyval3(poly, t);
    (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt()
}

pub fn poly_arc_length3(poly: &PolyR3, t0: f64, t1: f64) -> f64 {
    let (lo, hi) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
    let mut n: usize = 256.max(((hi - lo).abs() * 2048.0) as usize);
    n = n.max(2);
    n = (n + n % 2).max(2);
    let h = (hi - lo) / n as f64;
    let mut s = speed(poly, lo) + speed(poly, hi);
    for i in 1..n {
        let t = lo + h * i as f64;
        let w = if i % 2 == 0 { 2.0 } else { 4.0 };
        s += w * speed(poly, t);
    }
    s * h / 3.0
}

pub fn signed_poly_arc_length3(poly: &PolyR3, t0: f64, t1: f64) -> f64 {
    if t1 >= t0 {
        poly_arc_length3(poly, t0, t1)
    } else {
        -poly_arc_length3(poly, t1, t0)
    }
}

pub fn inv_poly_arc_length3(poly: &PolyR3, target_mm: f64) -> f64 {
    inv_poly_arc_from(poly, 0.0, target_mm, 1.0)
}

pub fn inv_poly_arc_from(poly: &PolyR3, t_start: f64, target_mm: f64, t_hi: f64) -> f64 {
    if target_mm <= 0.0 {
        return t_start;
    }
    let s_hi = signed_poly_arc_length3(poly, t_start, t_hi);
    if target_mm >= s_hi - 1e-6 {
        return t_hi;
    }
    let mut lo = t_start;
    let mut hi = t_hi;
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        let s = signed_poly_arc_length3(poly, t_start, mid);
        if s < target_mm {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

pub fn fit_param_poly_to_skeleton(skeleton: &[[f64; 3]], degree: usize) -> Option<PolyR3> {
    let n = skeleton.len();
    if n < degree + 1 {
        return None;
    }
    let mut total = 0.0;
    let mut deltas = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let dx = skeleton[i + 1][0] - skeleton[i][0];
        let dy = skeleton[i + 1][1] - skeleton[i][1];
        let dz = skeleton[i + 1][2] - skeleton[i][2];
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        deltas.push(len);
        total += len;
    }
    if total < 1e-12 {
        return None;
    }
    let mut t = vec![0.0_f64; n];
    let mut c = 0.0;
    for i in 1..n {
        c += deltas[i - 1];
        t[i] = c / total;
    }
    let mut v = DMatrix::zeros(n, degree + 1);
    for i in 0..n {
        for j in 0..=degree {
            v[(i, j)] = t[i].powi((degree - j) as i32);
        }
    }
    let qr = v.qr();
    let q = qr.q();
    let r = qr.r();
    let mut coeff = DMatrix::zeros(degree + 1, 3);
    for a in 0..3 {
        let b = DVector::from_iterator(n, skeleton.iter().map(|p| p[a]));
        let qtb = q.transpose() * &b;
        let col = r.solve_upper_triangular(&qtb)?;
        for r in 0..=degree {
            coeff[(r, a)] = col[r];
        }
    }
    Some(PolyR3 { degree, coeff })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_line_fit_and_arc() {
        let sk: [[f64; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let p = fit_param_poly_to_skeleton(&sk, 2).expect("lsq");
        let mid = polyval3(&p, 0.5);
        assert!((mid[0] - 1.0).abs() < 0.01, "mid[0]={}", mid[0]);
        let l = poly_arc_length3(&p, 0.0, 1.0);
        assert!((l - 2.0).abs() < 0.05, "L={l}");
    }

    #[test]
    fn degree_eight_fit_recovers_a_smooth_curve() {
        let n = 400;
        let sk: Vec<[f64; 3]> = (0..n)
            .map(|i| {
                let s = 50.0 * i as f64 / (n - 1) as f64;
                [3.0 * (s / 20.0).sin(), 0.01 * s * s, s]
            })
            .collect();
        let p = fit_param_poly_to_skeleton(&sk, 8).expect("lsq");
        let mut worst = 0.0f64;
        for q in &sk {
            let mut best = f64::INFINITY;
            for m in 0..=2000 {
                let pp = polyval3(&p, m as f64 / 2000.0);
                let d = ((pp[0] - q[0]).powi(2) + (pp[1] - q[1]).powi(2) + (pp[2] - q[2]).powi(2))
                    .sqrt();
                best = best.min(d);
            }
            worst = worst.max(best);
        }
        assert!(worst < 0.05, "worst {worst}");
        let flat = vec![[1.0, 2.0, 3.0]; 12];
        assert!(fit_param_poly_to_skeleton(&flat, 8).is_none());
    }

    #[test]
    fn inv_arc_straight() {
        let sk: [[f64; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let p = fit_param_poly_to_skeleton(&sk, 2).expect("lsq");
        let t_half = inv_poly_arc_length3(&p, 1.0);
        assert!((t_half - 0.5).abs() < 0.02, "t={t_half}");
    }
}
