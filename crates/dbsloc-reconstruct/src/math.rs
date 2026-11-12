// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! TRAC/CORE-oriented pure 3D math (no I/O).

use fastwarp_core::dot3;

/// `a - b`
#[inline]
pub fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// `a + b`
#[inline]
pub fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// `s * v`
#[inline]
pub fn scale3(v: [f64; 3], s: f64) -> [f64; 3] {
    [v[0] * s, v[1] * s, v[2] * s]
}

/// Euclidean length of `v`.
#[inline]
pub fn norm3(v: [f64; 3]) -> f64 {
    dot3(v, v).sqrt()
}

/// Cross product `a × b`.
#[inline]
pub fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Unit vector; zero input returns zero.
#[inline]
pub fn unit3(v: [f64; 3]) -> [f64; 3] {
    let n = norm3(v);
    if n < 1e-12 {
        return v;
    }
    scale3(v, 1.0 / n)
}

/// Like `unit3` but falls back to `fallback` if norm is tiny.
#[inline]
pub fn unit3_or(v: [f64; 3], fallback: [f64; 3]) -> [f64; 3] {
    let n = norm3(v);
    if n < 1e-6 {
        return fallback;
    }
    scale3(v, 1.0 / n)
}
