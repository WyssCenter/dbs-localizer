// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Ring fiducials in plane ⟂ shaft at `head` (specs/05; same geometry as `electrode_model.ring_in_plane`).

use crate::math::{add3, cross3, scale3, sub3, unit3, unit3_or};

use fastwarp_core::dot3;

/// Returns `(x, y)` ring points at `head`, radius `diameter_mm / 2`, plane perpendicular to `head→tail`.
pub fn ring_in_plane(
    head: [f64; 3],
    tail: [f64; 3],
    hint: Option<[f64; 3]>,
    diameter_mm: f64,
) -> ([f64; 3], [f64; 3]) {
    let t_hat = unit3(sub3(tail, head));
    let v0 = if let Some(h) = hint {
        if norm_hint(h) > 1e-9 {
            let dh = dot3(h, t_hat);
            let perp = [
                h[0] - t_hat[0] * dh,
                h[1] - t_hat[1] * dh,
                h[2] - t_hat[2] * dh,
            ];
            if norm_hint(perp) > 1e-3 {
                perp
            } else {
                aux_perp(t_hat)
            }
        } else {
            aux_perp(t_hat)
        }
    } else {
        aux_perp(t_hat)
    };
    let v0 = unit3_or(v0, [1.0, 0.0, 0.0]);
    let v1 = unit3(cross3(t_hat, v0));
    let r = 0.5 * diameter_mm;
    (add3(head, scale3(v0, r)), add3(head, scale3(v1, r)))
}

#[inline]
fn norm_hint(v: [f64; 3]) -> f64 {
    dot3(v, v).sqrt()
}

fn aux_perp(t_hat: [f64; 3]) -> [f64; 3] {
    let aux = if t_hat[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let d = dot3(aux, t_hat);
    [
        aux[0] - t_hat[0] * d,
        aux[1] - t_hat[1] * d,
        aux[2] - t_hat[2] * d,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::norm3;
    use fastwarp_core::dot3;

    #[test]
    fn ring_radius_and_orthogonality() {
        let head = [1.0, 2.0, 3.0];
        let tail = [1.0, 2.0, 8.0];
        let d = 1.28_f64;
        let (xx, yy) = ring_in_plane(head, tail, None, d);
        let t = unit3(sub3(tail, head));
        let r = 0.5 * d;
        for p in [xx, yy] {
            let dh = sub3(p, head);
            assert!((norm3(dh) - r).abs() < 1e-9);
            assert!(dot3(dh, t).abs() < 1e-9);
        }
        assert!(dot3(sub3(xx, head), sub3(yy, head)).abs() < 1e-9);
    }
}
