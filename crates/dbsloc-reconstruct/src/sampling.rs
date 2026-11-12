// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use ndarray::{Array3, Array4};

use crate::math::{cross3, norm3, scale3, sub3};
use fastwarp_core::{invert_rowmajor, Mat4};

pub fn arange(start: f64, stop: f64, step: f64) -> Vec<f64> {
    let n = ((stop - start) / step).ceil();
    if !n.is_finite() || n <= 0.0 {
        return Vec::new();
    }
    (0..n as usize).map(|i| start + i as f64 * step).collect()
}

struct Corners {
    i0: [usize; 3],
    i1: [usize; 3],
    d: [f64; 3],
}

fn corners(dims: [usize; 3], ijk: [f64; 3]) -> Corners {
    let mut i0 = [0usize; 3];
    let mut i1 = [0usize; 3];
    let mut d = [0.0f64; 3];
    for a in 0..3 {
        let hi = dims[a].saturating_sub(1) as f64;
        let q = ijk[a].clamp(0.0, hi);
        let base = (q.floor() as usize).min(dims[a].saturating_sub(2));
        i0[a] = base;
        d[a] = q - base as f64;
        i1[a] = (base + 1).min(dims[a].saturating_sub(1));
    }
    Corners { i0, i1, d }
}

fn corner_weights(c: &Corners) -> [([usize; 3], f64); 8] {
    let mut out = [([0usize; 3], 0.0f64); 8];
    let mut n = 0;
    for ci in 0..2 {
        for cj in 0..2 {
            for ck in 0..2 {
                let idx = [
                    if ci == 1 { c.i1[0] } else { c.i0[0] },
                    if cj == 1 { c.i1[1] } else { c.i0[1] },
                    if ck == 1 { c.i1[2] } else { c.i0[2] },
                ];
                let w = (if ci == 1 { c.d[0] } else { 1.0 - c.d[0] })
                    * (if cj == 1 { c.d[1] } else { 1.0 - c.d[1] })
                    * (if ck == 1 { c.d[2] } else { 1.0 - c.d[2] });
                out[n] = (idx, w);
                n += 1;
            }
        }
    }
    out
}

pub fn trilinear_sample(vol: &Array3<f64>, ijk: [f64; 3]) -> f64 {
    let (ni, nj, nk) = vol.dim();
    if ni == 0 || nj == 0 || nk == 0 {
        return 0.0;
    }
    let c = corners([ni, nj, nk], ijk);
    let mut out = 0.0;
    for (idx, w) in corner_weights(&c) {
        out += w * vol[[idx[0], idx[1], idx[2]]];
    }
    out
}

pub fn trilinear_sample_in_bounds(vol: &Array3<f64>, ijk: [f64; 3]) -> f64 {
    const EDGE_TOL_VOX: f64 = 1e-6;
    let (ni, nj, nk) = vol.dim();
    let dims = [ni, nj, nk];
    for a in 0..3 {
        if dims[a] == 0 {
            return f64::NAN;
        }
        let hi = (dims[a] - 1) as f64;
        if !(ijk[a] >= -EDGE_TOL_VOX && ijk[a] <= hi + EDGE_TOL_VOX) {
            return f64::NAN;
        }
    }
    trilinear_sample(vol, ijk)
}

pub fn trilinear_sample_vec(field: &Array4<f64>, ijk: [f64; 3]) -> Vec<f64> {
    let (ni, nj, nk, nc) = field.dim();
    let mut out = vec![0.0; nc];
    if ni == 0 || nj == 0 || nk == 0 {
        return out;
    }
    let c = corners([ni, nj, nk], ijk);
    for (idx, w) in corner_weights(&c) {
        for (ch, o) in out.iter_mut().enumerate() {
            *o += w * field[[idx[0], idx[1], idx[2], ch]];
        }
    }
    out
}

pub fn axis_and_disc_samples(
    vol: &Array3<f64>,
    aff: &Mat4,
    head: [f64; 3],
    tail: [f64; 3],
    step_mm: f64,
    disc_radius_mm: f64,
) -> (Vec<f64>, Vec<f64>) {
    axis_and_disc_with(
        vol,
        aff,
        head,
        tail,
        step_mm,
        disc_radius_mm,
        trilinear_sample,
    )
}

pub fn axis_and_disc_samples_in_bounds(
    vol: &Array3<f64>,
    aff: &Mat4,
    head: [f64; 3],
    tail: [f64; 3],
    step_mm: f64,
    disc_radius_mm: f64,
) -> (Vec<f64>, Vec<f64>) {
    axis_and_disc_with(
        vol,
        aff,
        head,
        tail,
        step_mm,
        disc_radius_mm,
        trilinear_sample_in_bounds,
    )
}

fn axis_and_disc_with(
    vol: &Array3<f64>,
    aff: &Mat4,
    head: [f64; 3],
    tail: [f64; 3],
    step_mm: f64,
    disc_radius_mm: f64,
    sampler: fn(&Array3<f64>, [f64; 3]) -> f64,
) -> (Vec<f64>, Vec<f64>) {
    let axis = sub3(tail, head);
    let n = norm3(axis);
    if n < 1e-9 {
        return (Vec::new(), Vec::new());
    }
    let Some(inv) = invert_rowmajor(aff) else {
        return (Vec::new(), Vec::new());
    };
    let u = scale3(axis, 1.0 / n);
    let a = if u[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let e1_raw = cross3(u, a);
    let e1 = scale3(e1_raw, 1.0 / norm3(e1_raw));
    let e2 = cross3(u, e1);
    let radii = arange(0.5 * step_mm, disc_radius_mm + 1e-9, 0.5 * step_mm);
    let angles = arange(0.0, 2.0 * std::f64::consts::PI, std::f64::consts::PI / 8.0);
    let mut offsets: Vec<[f64; 3]> = Vec::with_capacity(radii.len() * angles.len());
    for &r in &radii {
        for &t in &angles {
            let (s, c) = t.sin_cos();
            offsets.push([
                r * (c * e1[0] + s * e2[0]),
                r * (c * e1[1] + s * e2[1]),
                r * (c * e1[2] + s * e2[2]),
            ]);
        }
    }
    let sample = |p: [f64; 3]| sampler(vol, fastwarp_core::apply_rowmajor(&inv, p));
    let depths = arange(0.0, n + 1e-9, step_mm);
    let mut on_axis = Vec::with_capacity(depths.len());
    let mut disc = Vec::with_capacity(depths.len());
    for &dep in &depths {
        let c = [
            head[0] + dep * u[0],
            head[1] + dep * u[1],
            head[2] + dep * u[2],
        ];
        let v_axis = sample(c);
        let mut best = f64::NEG_INFINITY;
        for o in &offsets {
            let v = sample([c[0] + o[0], c[1] + o[1], c[2] + o[2]]);
            if v > best || v.is_nan() {
                best = v;
            }
        }
        on_axis.push(v_axis);
        disc.push(if best.is_nan() || v_axis.is_nan() {
            f64::NAN
        } else {
            best.max(v_axis)
        });
    }
    (on_axis, disc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trilinear_is_exact_for_a_linear_field() {
        let field = Array4::from_shape_fn((5, 6, 7, 3), |(i, j, k, c)| {
            let (i, j, k) = (i as f64, j as f64, k as f64);
            match c {
                0 => 2.0 * i - j + 0.5 * k + 1.0,
                1 => i + j + k,
                _ => 3.0 - k,
            }
        });
        for q in [
            [0.3, 4.7, 2.2],
            [3.9, 0.1, 5.5],
            [0.0, 0.0, 0.0],
            [4.0, 5.0, 6.0],
        ] {
            let got = trilinear_sample_vec(&field, q);
            let want = [
                2.0 * q[0] - q[1] + 0.5 * q[2] + 1.0,
                q[0] + q[1] + q[2],
                3.0 - q[2],
            ];
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() < 1e-12, "{q:?} {got:?} {want:?}");
            }
        }
    }

    #[test]
    fn samples_are_clamped_to_the_grid() {
        let vol = Array3::from_shape_fn((4, 4, 4), |(i, _, _)| i as f64);
        assert_eq!(trilinear_sample(&vol, [-5.0, 1.0, 1.0]), 0.0);
        assert_eq!(trilinear_sample(&vol, [9.0, 1.0, 1.0]), 3.0);
        assert!((trilinear_sample(&vol, [1.25, 1.0, 1.0]) - 1.25).abs() < 1e-12);
    }

    #[test]
    fn in_bounds_sampler_is_nan_outside_and_identical_inside() {
        let vol = Array3::from_shape_fn((4, 5, 6), |(i, j, k)| (i * 31 + j * 7 + k) as f64);
        for q in [
            [0.0, 0.0, 0.0],
            [3.0, 4.0, 5.0],
            [1.3, 2.7, 4.9],
            [2.5, 0.0, 5.0],
        ] {
            assert_eq!(
                trilinear_sample_in_bounds(&vol, q),
                trilinear_sample(&vol, q)
            );
        }
        for q in [
            [-0.5, 1.0, 1.0],
            [3.5, 1.0, 1.0],
            [1.0, 4.2, 1.0],
            [1.0, 1.0, -2.0],
            [f64::NAN, 1.0, 1.0],
        ] {
            assert!(trilinear_sample_in_bounds(&vol, q).is_nan(), "{q:?}");
        }
    }

    #[test]
    fn in_bounds_disc_samples_match_inside_and_flag_outside() {
        let mut vol = Array3::<f64>::zeros((20, 20, 20));
        for k in 0..20 {
            vol[[10, 11, k]] = 100.0;
        }
        let aff = fastwarp_core::identity_mat4();
        let inside =
            axis_and_disc_samples(&vol, &aff, [10.0, 10.0, 5.0], [10.0, 10.0, 11.0], 0.5, 1.5);
        let inside_b = axis_and_disc_samples_in_bounds(
            &vol,
            &aff,
            [10.0, 10.0, 5.0],
            [10.0, 10.0, 11.0],
            0.5,
            1.5,
        );
        assert_eq!(inside, inside_b);
        let (ax, disc) = axis_and_disc_samples_in_bounds(
            &vol,
            &aff,
            [10.0, 11.0, 15.0],
            [10.0, 11.0, 25.0],
            0.5,
            1.5,
        );
        assert!(ax[..8].iter().all(|v| *v == 100.0));
        assert!(ax[ax.len() - 4..].iter().all(|v| v.is_nan()));
        assert!(disc[disc.len() - 4..].iter().all(|v| v.is_nan()));
        let (ax_c, _) =
            axis_and_disc_samples(&vol, &aff, [10.0, 11.0, 15.0], [10.0, 11.0, 25.0], 0.5, 1.5);
        assert!(ax_c.iter().all(|v| *v == 100.0));
    }

    #[test]
    fn arange_matches_numpy_lengths() {
        assert_eq!(arange(0.25, 1.5 + 1e-9, 0.25).len(), 6);
        assert_eq!(
            arange(0.0, 2.0 * std::f64::consts::PI, std::f64::consts::PI / 8.0).len(),
            16
        );
        assert_eq!(arange(0.0, 6.0 + 1e-9, 0.5).len(), 13);
        assert!(arange(1.0, 0.0, 0.5).is_empty());
    }

    #[test]
    fn disc_maximum_finds_metal_beside_the_axis() {
        let mut vol = Array3::<f64>::zeros((20, 20, 20));
        for k in 0..20 {
            vol[[10, 11, k]] = 100.0;
        }
        let aff = fastwarp_core::identity_mat4();
        let (ax, disc) =
            axis_and_disc_samples(&vol, &aff, [10.0, 10.0, 5.0], [10.0, 10.0, 11.0], 0.5, 1.5);
        assert_eq!(ax.len(), 13);
        assert!(ax.iter().all(|v| *v == 0.0));
        assert!(disc.iter().all(|v| (*v - 100.0).abs() < 1e-9), "{disc:?}");
        let (ax0, d0) =
            axis_and_disc_samples(&vol, &aff, [1.0, 1.0, 1.0], [1.0, 1.0, 1.0], 0.5, 1.5);
        assert!(ax0.is_empty() && d0.is_empty());
    }
}
