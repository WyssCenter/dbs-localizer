// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Intensity contact detection — `specs/10_pacer.md` §8.

/// Zero-phase box smoothing (forward then backward).
#[allow(clippy::needless_range_loop)]
pub fn filtfilt_box(y: &[f32], w: usize) -> Vec<f32> {
    if y.is_empty() {
        return vec![];
    }
    if w <= 1 {
        return y.to_vec();
    }
    let n = y.len();
    let mut a = y.to_vec();
    for _ in 0..2 {
        let mut s = a.clone();
        for i in 0..n {
            let i0 = i.saturating_sub(w / 2);
            let i1 = (i + w / 2 + 1).min(n);
            let mut sum = 0.0f32;
            let mut c = 0u32;
            for j in i0..i1 {
                sum += a[j];
                c += 1;
            }
            s[i] = if c > 0 { sum / c as f32 } else { 0.0 };
        }
        a = s;
    }
    a
}

/// Local maxima in (s_mm, y) with min spacing in **mm** (1.4) and height/prominence.
pub fn find_peaks_mm(s_mm: &[f32], y: &[f32], max_s: f32) -> (Vec<usize>, Vec<f32>) {
    if s_mm.len() != y.len() || s_mm.is_empty() {
        return (vec![], vec![]);
    }
    let m = nmean(y);
    if !m.is_finite() || m <= 0.0 {
        return (vec![], vec![]);
    }
    let hmin = 1.1f32 * m;
    let pmin = 0.01f32 * m;
    let mut cands: Vec<usize> = vec![];
    for i in 1..(y.len() - 1) {
        if s_mm[i] > max_s {
            break;
        }
        if y[i] >= hmin && y[i] > y[i - 1] && y[i] > y[i + 1] {
            let pro = (y[i] - y[i - 1].min(y[i + 1])).min(y[i] - 0.0);
            if pro >= pmin {
                cands.push(i);
            }
        }
    }
    cands.sort_by(|&a, &b| s_mm[a].total_cmp(&s_mm[b]));
    let mut kept: Vec<usize> = vec![];
    for &i in &cands {
        let good = !kept.iter().any(|&j| (s_mm[i] - s_mm[j]).abs() < 1.4f32);
        if good {
            kept.push(i);
        }
    }
    let vals: Vec<f32> = kept.iter().map(|&i| s_mm[i]).collect();
    (kept, vals)
}

fn nmean(x: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    let mut c = 0u32;
    for v in x {
        if v.is_finite() {
            sum += *v;
            c += 1;
        }
    }
    if c == 0 {
        0.0
    } else {
        sum / c as f32
    }
}

/// Contact-area fallback: threshold at local mean, second run = contact band.
pub fn contact_area_fallback(s_mm: &[f32], y: &[f32], max_s: f32) -> (f32, f32) {
    let m = nmean(
        &y.iter()
            .zip(s_mm)
            .filter(|(_, s)| **s <= max_s)
            .map(|(v, _)| *v)
            .collect::<Vec<_>>(),
    );
    let th = m;
    let labels: Vec<i32> = s_mm
        .iter()
        .zip(y)
        .map(|(s, v)| {
            if *s > max_s {
                -1
            } else if *v < th {
                1
            } else {
                0
            }
        })
        .collect();
    let mut runs: Vec<(usize, usize)> = vec![];
    let mut k = 0;
    while k < labels.len() {
        if labels[k] != 0 {
            k += 1;
            continue;
        }
        let s0 = k;
        while k < labels.len() && labels[k] == 0 {
            k += 1;
        }
        runs.push((s0, k - 1));
    }
    if runs.len() < 2 {
        return (f32::NAN, f32::NAN);
    }
    let (a, b) = runs[1];
    let c = 0.5 * (s_mm[a] + s_mm[b]);
    let w = (s_mm[b] - s_mm[a]).abs();
    (c, w)
}
