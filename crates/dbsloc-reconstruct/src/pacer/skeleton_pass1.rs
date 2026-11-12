// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! First-pass axial skeleton + degree-8 fit — `specs/10_pacer.md` §6.

use super::errors::PacerError;
use super::poly3::{fit_param_poly_to_skeleton, PolyR3};

const PLANE_TOL_MM: f64 = 0.1;
const INTERNAL_DEG: usize = 8;

/// Pick axis 0,1,2 = x,y,z in world mm with most distinct plane coordinates.
pub fn pick_stack_axis(points_mm: &[[f64; 3]]) -> usize {
    use std::collections::BTreeSet;
    let count_axis = |a: usize| {
        let mut s = BTreeSet::new();
        for p in points_mm {
            let x = (p[a] / PLANE_TOL_MM).round() as i64;
            s.insert(x);
        }
        s.len()
    };
    let c0 = count_axis(0);
    let c1 = count_axis(1);
    let c2 = count_axis(2);
    if c0 >= c1 && c0 >= c2 {
        0
    } else if c1 >= c2 {
        1
    } else {
        2
    }
}

/// Intensity-weighted centroids per plane, filter, poly fit.
pub fn first_pass_poly(
    points_mm: &[[f64; 3]],
    values: &[f32],
    stack_axis: usize,
    reverse_dir: bool,
) -> Result<PolyR3, PacerError> {
    if points_mm.len() != values.len() || points_mm.len() < 3 {
        return Err(PacerError::ObliqueCt);
    }
    use std::collections::BTreeMap;
    type Key = i64;
    let mut groups: BTreeMap<Key, Vec<usize>> = BTreeMap::new();
    for (idx, p) in points_mm.iter().enumerate() {
        let k = (p[stack_axis] / PLANE_TOL_MM).round() as i64;
        groups.entry(k).or_default().push(idx);
    }
    let mut skel_w: Vec<([f64; 3], f32)> = vec![];
    for ixs in groups.values() {
        if ixs.len() < 2 {
            continue;
        }
        let wsum: f32 = ixs.iter().map(|&i| values[i].max(0.0)).sum();
        if wsum < 1e-6 {
            continue;
        }
        let mut c = [0.0; 3];
        for &i in ixs {
            let wi = values[i].max(0.0) as f64;
            c[0] += points_mm[i][0] * wi;
            c[1] += points_mm[i][1] * wi;
            c[2] += points_mm[i][2] * wi;
        }
        c[0] /= wsum as f64;
        c[1] /= wsum as f64;
        c[2] /= wsum as f64;
        skel_w.push((c, wsum));
    }
    if skel_w.len() < 2 {
        return Err(PacerError::ObliqueCt);
    }
    let med = {
        let mut s: Vec<f32> = skel_w.iter().map(|(_, w)| *w).collect();
        s.sort_by(|a, b| a.total_cmp(b));
        s[s.len() / 2]
    };
    let mut skeleton: Vec<[f64; 3]> = skel_w
        .into_iter()
        .filter(|(_pt, w)| *w >= med / 1.5)
        .map(|(pt, _w)| pt)
        .collect();
    if skeleton.len() < 2 {
        return Err(PacerError::EmptySkeleton);
    }
    skeleton.sort_by(|a, b| a[stack_axis].total_cmp(&b[stack_axis]));
    if let (Some(first), Some(last)) = (skeleton.first(), skeleton.last()) {
        if first[2] > last[2] {
            skeleton.reverse();
        }
    }
    if reverse_dir {
        skeleton.reverse();
    }
    let deg = (skeleton.len() - 1).min(INTERNAL_DEG);
    let poly = fit_param_poly_to_skeleton(&skeleton, deg).ok_or(PacerError::EmptySkeleton)?;
    Ok(poly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacer::poly3::polyval3;

    fn lead_cloud(tip: [f64; 3], u: [f64; 3], len_mm: f64) -> (Vec<[f64; 3]>, Vec<f32>) {
        let mut pts = vec![];
        let mut vals = vec![];
        let n = (len_mm / 0.25) as usize;
        for s in 0..=n {
            let a = s as f64 * 0.25;
            for (dx, dy) in [(0.0, 0.0), (0.3, 0.0), (-0.3, 0.0), (0.0, 0.3), (0.0, -0.3)] {
                pts.push([
                    tip[0] + u[0] * a + dx,
                    tip[1] + u[1] * a + dy,
                    tip[2] + u[2] * a,
                ]);
                vals.push(2500.0);
            }
        }
        (pts, vals)
    }

    #[test]
    fn skeleton_starts_at_the_deep_end_whatever_the_stack_axis() {
        let n = (0.9f64 * 0.9 + 0.2 * 0.2 + 0.39 * 0.39).sqrt();
        for u in [
            [0.9 / n, 0.2 / n, 0.39 / n],
            [-0.9 / n, 0.2 / n, 0.39 / n],
            [0.1, 0.05, (1.0f64 - 0.0125).sqrt()],
        ] {
            let tip = [-5.0, 3.0, -20.0];
            let (pts, vals) = lead_cloud(tip, u, 45.0);
            let axis = pick_stack_axis(&pts);
            let poly = first_pass_poly(&pts, &vals, axis, false).expect("poly");
            let p0 = polyval3(&poly, 0.0);
            let p1 = polyval3(&poly, 1.0);
            assert!(
                p0[2] < p1[2],
                "axis {axis}: t=0 at z {} above t=1 at z {}",
                p0[2],
                p1[2]
            );
            let rev = first_pass_poly(&pts, &vals, axis, true).expect("poly");
            assert!(polyval3(&rev, 0.0)[2] > polyval3(&rev, 1.0)[2]);
        }
    }
}
