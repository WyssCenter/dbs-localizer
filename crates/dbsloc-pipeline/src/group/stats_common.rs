// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Textbook statistics shared by the group explorers (`specs/14` §7, §8, §9).
//!
//! Everything is generic over slices of `f64`; NaN marks a missing value where a function says so.
//! Degenerate input (too few values, no variance) gives NaN, never a panic.

use serde::{Deserialize, Serialize};

/// Result of a hypothesis test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestResult {
    pub stat: f64,
    pub p: f64,
}

impl TestResult {
    /// A result with no value.
    pub const NAN: TestResult = TestResult {
        stat: f64::NAN,
        p: f64::NAN,
    };
}

/// Mean of the values; NaN for an empty slice.
pub fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        return f64::NAN;
    }
    x.iter().sum::<f64>() / x.len() as f64
}

/// Sample variance (n - 1); NaN below two values.
pub fn variance(x: &[f64]) -> f64 {
    if x.len() < 2 {
        return f64::NAN;
    }
    let m = mean(x);
    x.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / (x.len() - 1) as f64
}

/// Sample standard deviation (n - 1).
pub fn sd(x: &[f64]) -> f64 {
    variance(x).sqrt()
}

/// Median; NaN for an empty slice.
pub fn median(x: &[f64]) -> f64 {
    if x.is_empty() {
        return f64::NAN;
    }
    let mut v = x.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Ranks starting at 1, ties get the average rank.
pub fn ranks_average(x: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..x.len()).collect();
    idx.sort_by(|&a, &b| x[a].total_cmp(&x[b]));
    let mut ranks = vec![0.0; x.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && x[idx[j + 1]] == x[idx[i]] {
            j += 1;
        }
        let r = 0.5 * ((i + 1) + (j + 1)) as f64;
        for &k in &idx[i..=j] {
            ranks[k] = r;
        }
        i = j + 1;
    }
    ranks
}

/// Natural log of the gamma function (Lanczos).
pub fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

fn gamma_p_series(a: f64, x: f64) -> f64 {
    let mut ap = a;
    let mut sum = 1.0 / a;
    let mut del = sum;
    for _ in 0..1000 {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * 1e-16 {
            break;
        }
    }
    sum * (-x + a * x.ln() - ln_gamma(a)).exp()
}

fn gamma_q_cf(a: f64, x: f64) -> f64 {
    let tiny = 1e-300;
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / tiny;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..1000 {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < tiny {
            d = tiny;
        }
        c = b + an / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-16 {
            break;
        }
    }
    (-x + a * x.ln() - ln_gamma(a)).exp() * h
}

/// Regularised upper incomplete gamma function Q(a, x).
pub fn gamma_q(a: f64, x: f64) -> f64 {
    if x.is_nan() || a <= 0.0 || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        1.0 - gamma_p_series(a, x)
    } else {
        gamma_q_cf(a, x)
    }
}

/// Complementary error function.
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x >= 0.0 {
        gamma_q(0.5, x * x)
    } else {
        2.0 - gamma_q(0.5, x * x)
    }
}

/// Standard normal cumulative distribution function.
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

/// Standard normal quantile (Acklam's approximation refined by one Halley step).
pub fn norm_inv_cdf(p: f64) -> f64 {
    if p.is_nan() || !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if p == 0.0 {
        return f64::NEG_INFINITY;
    }
    if p == 1.0 {
        return f64::INFINITY;
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let lo = 0.024_25;
    let x = if p < lo {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - lo {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    let e = norm_cdf(x) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    let tiny = 1e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < tiny {
        d = tiny;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..1000 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < tiny {
            d = tiny;
        }
        c = 1.0 + aa / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < tiny {
            d = tiny;
        }
        c = 1.0 + aa / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

/// Regularised incomplete beta function I_x(a, b).
pub fn reg_inc_beta(a: f64, b: f64, x: f64) -> f64 {
    if x.is_nan() || !(0.0..=1.0).contains(&x) {
        return f64::NAN;
    }
    if x == 0.0 || x == 1.0 {
        return x;
    }
    let bt = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * beta_cf(a, b, x) / a
    } else {
        1.0 - bt * beta_cf(b, a, 1.0 - x) / b
    }
}

/// Two-sided p of a Student t statistic with `df` degrees of freedom.
pub fn t_two_sided_p(t: f64, df: f64) -> f64 {
    if t.is_nan() || df.is_nan() || df <= 0.0 {
        return f64::NAN;
    }
    if t.is_infinite() {
        return 0.0;
    }
    reg_inc_beta(df / 2.0, 0.5, df / (df + t * t))
}

/// Upper tail of the chi-square distribution with one degree of freedom.
pub fn chi2_1_sf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x <= 0.0 {
        return 1.0;
    }
    erfc((x / 2.0).sqrt())
}

/// One-sample t-test of the mean against `mu`; infinite or undefined t gives NaN.
pub fn t_test_one_sample(x: &[f64], mu: f64) -> TestResult {
    let n = x.len();
    if n < 2 {
        return TestResult::NAN;
    }
    let s = sd(x);
    let t = (mean(x) - mu) / (s / (n as f64).sqrt());
    if !t.is_finite() {
        return TestResult::NAN;
    }
    TestResult {
        stat: t,
        p: t_two_sided_p(t, (n - 1) as f64),
    }
}

/// Paired t-test of `x` against `y` (equal lengths).
pub fn t_test_paired(x: &[f64], y: &[f64]) -> TestResult {
    if x.len() != y.len() {
        return TestResult::NAN;
    }
    let d: Vec<f64> = x.iter().zip(y).map(|(a, b)| a - b).collect();
    t_test_one_sample(&d, 0.0)
}

/// Student two-sample t-test with equal variances.
pub fn t_test_two_sample(x: &[f64], y: &[f64]) -> TestResult {
    let (n1, n2) = (x.len(), y.len());
    if n1 < 1 || n2 < 1 || n1 + n2 < 3 {
        return TestResult::NAN;
    }
    let df = (n1 + n2 - 2) as f64;
    let ss = |v: &[f64]| {
        let m = mean(v);
        v.iter().map(|a| (a - m) * (a - m)).sum::<f64>()
    };
    let pooled = (ss(x) + ss(y)) / df;
    let t = (mean(x) - mean(y)) / (pooled * (1.0 / n1 as f64 + 1.0 / n2 as f64)).sqrt();
    if !t.is_finite() {
        return TestResult::NAN;
    }
    TestResult {
        stat: t,
        p: t_two_sided_p(t, df),
    }
}

/// Wilcoxon signed-rank test of the differences `d` (zeros dropped), two-sided. The exact
/// distribution is used for up to 15 non-zero differences without ties, otherwise the normal
/// approximation with tie and continuity correction. The statistic is W+ (sum of positive ranks).
pub fn wilcoxon_signed_rank(d: &[f64]) -> TestResult {
    let v: Vec<f64> = d
        .iter()
        .copied()
        .filter(|x| x.is_finite() && *x != 0.0)
        .collect();
    let n = v.len();
    if n == 0 {
        return TestResult { stat: 0.0, p: 1.0 };
    }
    let abs: Vec<f64> = v.iter().map(|x| x.abs()).collect();
    let ranks = ranks_average(&abs);
    let w: f64 = v
        .iter()
        .zip(&ranks)
        .filter(|(x, _)| **x > 0.0)
        .map(|(_, r)| r)
        .sum();
    let ties = {
        let mut s = abs.clone();
        s.sort_by(f64::total_cmp);
        s.windows(2).any(|p| p[0] == p[1])
    };
    let nf = n as f64;
    let total = nf * (nf + 1.0) / 2.0;
    if n <= 15 && !ties {
        let max = n * (n + 1) / 2;
        let mut counts = vec![0f64; max + 1];
        counts[0] = 1.0;
        for r in 1..=n {
            for s in (r..=max).rev() {
                counts[s] += counts[s - r];
            }
        }
        let all = 2f64.powi(n as i32);
        let wi = w.round() as usize;
        let lower: f64 = counts[..=wi.min(max)].iter().sum::<f64>() / all;
        let upper: f64 = counts[wi.min(max)..].iter().sum::<f64>() / all;
        return TestResult {
            stat: w,
            p: (2.0 * lower.min(upper)).min(1.0),
        };
    }
    let mut tie_term = 0.0;
    let mut sorted = abs.clone();
    sorted.sort_by(f64::total_cmp);
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1] == sorted[i] {
            j += 1;
        }
        let t = (j - i + 1) as f64;
        tie_term += t * t * t - t;
        i = j + 1;
    }
    let var = nf * (nf + 1.0) * (2.0 * nf + 1.0) / 24.0 - tie_term / 48.0;
    if var <= 0.0 {
        return TestResult { stat: w, p: 1.0 };
    }
    let diff = w - total / 2.0;
    let z = (diff.abs() - 0.5).max(0.0) / var.sqrt();
    TestResult {
        stat: w,
        p: (2.0 * (1.0 - norm_cdf(z))).min(1.0),
    }
}

/// Wilcoxon rank-sum (Mann-Whitney) test, normal approximation with tie and continuity
/// correction. The statistic is the rank sum of `x`.
pub fn wilcoxon_rank_sum(x: &[f64], y: &[f64]) -> TestResult {
    let (n1, n2) = (x.len(), y.len());
    if n1 == 0 || n2 == 0 {
        return TestResult::NAN;
    }
    let all: Vec<f64> = x.iter().chain(y).copied().collect();
    let ranks = ranks_average(&all);
    let w: f64 = ranks[..n1].iter().sum();
    let (n1f, n2f) = (n1 as f64, n2 as f64);
    let n = n1f + n2f;
    let mut sorted = all;
    sorted.sort_by(f64::total_cmp);
    let mut tie_term = 0.0;
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1] == sorted[i] {
            j += 1;
        }
        let t = (j - i + 1) as f64;
        tie_term += t * t * t - t;
        i = j + 1;
    }
    let var = n1f * n2f / 12.0 * ((n + 1.0) - tie_term / (n * (n - 1.0)));
    if var <= 0.0 {
        return TestResult { stat: w, p: 1.0 };
    }
    let diff = w - n1f * (n + 1.0) / 2.0;
    let z = (diff.abs() - 0.5).max(0.0) / var.sqrt();
    TestResult {
        stat: w,
        p: (2.0 * (1.0 - norm_cdf(z))).min(1.0),
    }
}

/// Pearson correlation; NaN when either side has no variance or fewer than two pairs.
pub fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len();
    if n < 2 || n != y.len() {
        return f64::NAN;
    }
    let (mx, my) = (mean(x), mean(y));
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (a, b) in x.iter().zip(y) {
        sxy += (a - mx) * (b - my);
        sxx += (a - mx) * (a - mx);
        syy += (b - my) * (b - my);
    }
    let d = (sxx * syy).sqrt();
    if d == 0.0 {
        f64::NAN
    } else {
        (sxy / d).clamp(-1.0, 1.0)
    }
}

/// Spearman rank correlation (average ranks).
pub fn spearman(x: &[f64], y: &[f64]) -> f64 {
    if x.len() != y.len() {
        return f64::NAN;
    }
    pearson(&ranks_average(x), &ranks_average(y))
}

/// Two-sided p of a correlation `r` from `n` pairs (t distribution with n - 2 degrees).
pub fn correlation_p(r: f64, n: usize) -> f64 {
    if r.is_nan() || n < 3 {
        return f64::NAN;
    }
    if r.abs() >= 1.0 {
        return 0.0;
    }
    let df = (n - 2) as f64;
    t_two_sided_p(r * (df / (1.0 - r * r)).sqrt(), df)
}

/// Fisher z transform (atanh) of a correlation, clamped just inside +-1 so it stays finite; NaN
/// stays NaN.
pub fn fisher_z(r: f64) -> f64 {
    if r.is_nan() {
        f64::NAN
    } else {
        r.clamp(-0.999_999_999_9, 0.999_999_999_9).atanh()
    }
}

/// Share of null values whose magnitude is at least `|observed|` (the permutation p).
pub fn permutation_p(observed: f64, null: &[f64]) -> f64 {
    if observed.is_nan() || null.is_empty() {
        return f64::NAN;
    }
    let o = observed.abs() - 1e-12;
    null.iter().filter(|v| v.abs() >= o).count() as f64 / null.len() as f64
}

/// The `q` quantile (0 to 1) of `x` by linear interpolation of order statistics.
pub fn quantile(x: &[f64], q: f64) -> f64 {
    let mut v: Vec<f64> = x.iter().copied().filter(|a| !a.is_nan()).collect();
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    let pos = q.clamp(0.0, 1.0) * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
}

/// Multiple-comparison adjustment in the reference form (`specs/14` §7.4): `p * n / rank` with
/// ascending ranks and no monotone step-up minimum.
pub fn fdr_reference(p: &[f64]) -> Vec<f64> {
    let n = p.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| p[a].total_cmp(&p[b]));
    let mut out = vec![0.0; n];
    for (rank, &i) in idx.iter().enumerate() {
        out[i] = p[i] * n as f64 / (rank + 1) as f64;
    }
    out
}

/// Benjamini-Hochberg adjusted p-values (with the monotone step-up minimum).
pub fn fdr_benjamini_hochberg(p: &[f64]) -> Vec<f64> {
    let n = p.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| p[a].total_cmp(&p[b]));
    let mut out = vec![0.0; n];
    let mut running = f64::INFINITY;
    for (rank, &i) in idx.iter().enumerate().rev() {
        running = running.min(p[i] * n as f64 / (rank + 1) as f64);
        out[i] = running.min(1.0);
    }
    out
}

/// Bonferroni adjusted p-values (not clamped).
pub fn bonferroni(p: &[f64]) -> Vec<f64> {
    let n = p.len() as f64;
    p.iter().map(|v| v * n).collect()
}

#[allow(clippy::needless_range_loop)]
/// Solves `a x = b` by Gaussian elimination with partial pivoting; `None` when singular.
pub fn solve_linear(a: &[Vec<f64>], b: &[f64]) -> Option<Vec<f64>> {
    let n = b.len();
    let mut m: Vec<Vec<f64>> = a
        .iter()
        .zip(b)
        .map(|(row, v)| {
            let mut r = row.clone();
            r.push(*v);
            r
        })
        .collect();
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        for r in 0..n {
            if r != col {
                let f = m[r][col] / m[col][col];
                for c in col..=n {
                    let v = m[col][c];
                    m[r][c] -= f * v;
                }
            }
        }
    }
    Some((0..n).map(|i| m[i][n] / m[i][i]).collect())
}

#[allow(clippy::needless_range_loop)]
/// Ordinary least squares of `y` on the columns of `x` (each `x[j]` has one value per row),
/// optionally with an intercept (returned first). Rows with a NaN anywhere are skipped.
pub fn ols_fit(y: &[f64], x: &[Vec<f64>], intercept: bool) -> Option<Vec<f64>> {
    let rows: Vec<usize> = (0..y.len())
        .filter(|&i| y[i].is_finite() && x.iter().all(|c| c[i].is_finite()))
        .collect();
    let k = x.len() + usize::from(intercept);
    if rows.len() < k || k == 0 {
        return None;
    }
    let design = |i: usize, j: usize| {
        if intercept {
            if j == 0 {
                1.0
            } else {
                x[j - 1][i]
            }
        } else {
            x[j][i]
        }
    };
    let mut xtx = vec![vec![0.0; k]; k];
    let mut xty = vec![0.0; k];
    for &i in &rows {
        for a in 0..k {
            xty[a] += design(i, a) * y[i];
            for b in 0..k {
                xtx[a][b] += design(i, a) * design(i, b);
            }
        }
    }
    solve_linear(&xtx, &xty)
}

fn ols_apply(beta: &[f64], y: &[f64], x: &[Vec<f64>]) -> Vec<f64> {
    (0..y.len())
        .map(|i| {
            if y[i].is_finite() && x.iter().all(|c| c[i].is_finite()) {
                let fit: f64 = beta[0]
                    + x.iter()
                        .enumerate()
                        .map(|(j, c)| beta[j + 1] * c[i])
                        .sum::<f64>();
                y[i] - fit
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// Residuals of `y` after an OLS fit on `x` with an intercept; NaN rows stay NaN. With no
/// usable fit `y` is returned unchanged.
pub fn ols_residuals(y: &[f64], x: &[Vec<f64>]) -> Vec<f64> {
    match ols_fit(y, x, true) {
        Some(beta) => ols_apply(&beta, y, x),
        None => y.to_vec(),
    }
}

/// Residuals of `y` after an OLS fit on `covariates` with an intercept. Rows with a non-finite
/// value in `y` or any covariate stay NaN; with no residual degrees of freedom or a singular
/// design every row is NaN.
pub fn covariate_residuals(y: &[f64], covariates: &[Vec<f64>]) -> Vec<f64> {
    let usable = (0..y.len())
        .filter(|&i| y[i].is_finite() && covariates.iter().all(|c| c[i].is_finite()))
        .count();
    if usable <= covariates.len() + 1 {
        return vec![f64::NAN; y.len()];
    }
    match ols_fit(y, covariates, true) {
        Some(beta) => ols_apply(&beta, y, covariates),
        None => vec![f64::NAN; y.len()],
    }
}

/// Least-squares line `y = b0 + b1 x` over the finite pairs, returned as `(b0, b1)`; `None` below
/// two pairs or for a constant `x`.
pub fn linear_regression(x: &[f64], y: &[f64]) -> Option<(f64, f64)> {
    let (xs, ys): (Vec<f64>, Vec<f64>) = x
        .iter()
        .zip(y)
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .map(|(a, b)| (*a, *b))
        .unzip();
    if xs.len() < 2 {
        return None;
    }
    let (mx, my) = (mean(&xs), mean(&ys));
    let sxx: f64 = xs.iter().map(|a| (a - mx) * (a - mx)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let sxy: f64 = xs.iter().zip(&ys).map(|(a, b)| (a - mx) * (b - my)).sum();
    let b1 = sxy / sxx;
    Some((my - b1 * mx, b1))
}

/// z-scores of the finite values (sample sd); NaN stays NaN; constant input gives zeros.
pub fn zscore(x: &[f64]) -> Vec<f64> {
    let fin: Vec<f64> = x.iter().copied().filter(|v| v.is_finite()).collect();
    let m = mean(&fin);
    let s = sd(&fin);
    x.iter()
        .map(|v| {
            if !v.is_finite() {
                f64::NAN
            } else if s > 0.0 && s.is_finite() {
                (v - m) / s
            } else {
                0.0
            }
        })
        .collect()
}

/// Rank-based inverse normal transform (van Albada and Robinson 2007): `Phi^-1(rank / (n + 1))`
/// with average ranks over the finite values.
pub fn van_albada(x: &[f64]) -> Vec<f64> {
    let idx: Vec<usize> = (0..x.len()).filter(|&i| x[i].is_finite()).collect();
    let vals: Vec<f64> = idx.iter().map(|&i| x[i]).collect();
    let ranks = ranks_average(&vals);
    let n = vals.len() as f64;
    let mut out = vec![f64::NAN; x.len()];
    for (k, &i) in idx.iter().enumerate() {
        out[i] = norm_inv_cdf(ranks[k] / (n + 1.0));
    }
    out
}

fn ln_choose(n: u64, k: u64) -> f64 {
    ln_gamma(n as f64 + 1.0) - ln_gamma(k as f64 + 1.0) - ln_gamma((n - k) as f64 + 1.0)
}

/// Probability mass of Binomial(m, p0) at `k`.
pub fn binomial_pmf(k: u64, m: u64, p0: f64) -> f64 {
    if k > m {
        return 0.0;
    }
    if p0 <= 0.0 {
        return f64::from(k == 0);
    }
    if p0 >= 1.0 {
        return f64::from(k == m);
    }
    (ln_choose(m, k) + k as f64 * p0.ln() + (m - k) as f64 * (1.0 - p0).ln()).exp()
}

/// Exact two-sided binomial test: the sum of all outcome probabilities not larger than that of
/// `k` (with a relative tolerance of 1e-7).
pub fn binomial_two_sided_p(k: u64, m: u64, p0: f64) -> f64 {
    if k > m || !(0.0..=1.0).contains(&p0) {
        return f64::NAN;
    }
    let pk = binomial_pmf(k, m, p0);
    let lim = pk * (1.0 + 1e-7);
    (0..=m)
        .map(|i| binomial_pmf(i, m, p0))
        .filter(|p| *p <= lim)
        .sum::<f64>()
        .min(1.0)
}

/// Chi-square test of two proportions `k1 / n1` and `k0 / n0` with Yates continuity correction.
/// The statistic is signed by `k1 / n1 - k0 / n0`; p from the chi-square distribution (1 df).
pub fn chi2_two_proportions_yates(k1: u64, n1: u64, k0: u64, n0: u64) -> TestResult {
    if n1 == 0 || n0 == 0 || k1 > n1 || k0 > n0 {
        return TestResult::NAN;
    }
    let a = k1 as f64;
    let b = (n1 - k1) as f64;
    let c = k0 as f64;
    let d = (n0 - k0) as f64;
    let n = a + b + c + d;
    let denom = (a + b) * (c + d) * (a + c) * (b + d);
    if denom == 0.0 {
        return TestResult { stat: 0.0, p: 1.0 };
    }
    let num = ((a * d - b * c).abs() - n / 2.0).max(0.0);
    let chi2 = n * num * num / denom;
    let sign = k1 as f64 / n1 as f64 - k0 as f64 / n0 as f64;
    TestResult {
        stat: if sign < 0.0 { -chi2 } else { chi2 },
        p: chi2_1_sf(chi2),
    }
}

/// Area under the ROC curve of `score` for the binary `label` (> 0.5 = positive), ties half.
pub fn auc(score: &[f64], label: &[f64]) -> f64 {
    let pos: Vec<f64> = score
        .iter()
        .zip(label)
        .filter(|(s, l)| s.is_finite() && **l > 0.5)
        .map(|(s, _)| *s)
        .collect();
    let neg: Vec<f64> = score
        .iter()
        .zip(label)
        .filter(|(s, l)| s.is_finite() && l.is_finite() && **l <= 0.5)
        .map(|(s, _)| *s)
        .collect();
    if pos.is_empty() || neg.is_empty() {
        return f64::NAN;
    }
    let mut wins = 0.0;
    for p in &pos {
        for q in &neg {
            if p > q {
                wins += 1.0;
            } else if p == q {
                wins += 0.5;
            }
        }
    }
    wins / (pos.len() * neg.len()) as f64
}

/// Logistic regression of the binary `y` on one predictor (Newton iterations); `(b0, b1)`.
pub fn logistic_fit(x: &[f64], y: &[f64]) -> Option<(f64, f64)> {
    let rows: Vec<usize> = (0..x.len())
        .filter(|&i| x[i].is_finite() && y[i].is_finite())
        .collect();
    if rows.len() < 3 {
        return None;
    }
    let m = mean(&rows.iter().map(|&i| x[i]).collect::<Vec<_>>());
    let s = sd(&rows.iter().map(|&i| x[i]).collect::<Vec<_>>());
    if s.is_nan() || s <= 0.0 {
        return None;
    }
    let z: Vec<f64> = rows.iter().map(|&i| (x[i] - m) / s).collect();
    let t: Vec<f64> = rows.iter().map(|&i| f64::from(y[i] > 0.5)).collect();
    let (mut b0, mut b1) = (0.0f64, 0.0f64);
    for _ in 0..50 {
        let (mut g0, mut g1, mut h00, mut h01, mut h11) = (0.0, 0.0, 1e-6, 0.0, 1e-6);
        for (zi, ti) in z.iter().zip(&t) {
            let p = 1.0 / (1.0 + (-(b0 + b1 * zi)).exp());
            let w = (p * (1.0 - p)).max(1e-9);
            g0 += ti - p;
            g1 += (ti - p) * zi;
            h00 += w;
            h01 += w * zi;
            h11 += w * zi * zi;
        }
        let det = h00 * h11 - h01 * h01;
        if det.abs() < 1e-18 {
            break;
        }
        let d0 = (h11 * g0 - h01 * g1) / det;
        let d1 = (-h01 * g0 + h00 * g1) / det;
        b0 += d0.clamp(-5.0, 5.0);
        b1 += d1.clamp(-5.0, 5.0);
        if d0.abs() < 1e-9 && d1.abs() < 1e-9 {
            break;
        }
    }
    let slope = b1 / s;
    Some((b0 - slope * m, slope))
}

/// AUC of a logistic regression of the binary `label` on `score` (in-sample).
pub fn logistic_auc(score: &[f64], label: &[f64]) -> f64 {
    let raw = auc(score, label);
    match logistic_fit(score, label) {
        Some((_, b1)) if b1 < 0.0 => 1.0 - raw,
        _ => raw,
    }
}

/// Correlation flavour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CorrKind {
    Spearman,
    Pearson,
    Bend,
}

impl CorrKind {
    /// Parses `spearman`, `pearson` or `bend`.
    pub fn parse(text: &str) -> Option<CorrKind> {
        match text.trim().to_ascii_lowercase().as_str() {
            "spearman" => Some(CorrKind::Spearman),
            "pearson" => Some(CorrKind::Pearson),
            "bend" => Some(CorrKind::Bend),
            _ => None,
        }
    }
}

/// Multiple-comparison correction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Correction {
    Uncorrected,
    Fdr,
    Bonferroni,
}

impl Correction {
    /// Parses `uncorrected`, `fdr` or `bonferroni`.
    pub fn parse(text: &str) -> Option<Correction> {
        match text.trim().to_ascii_lowercase().as_str() {
            "uncorrected" | "none" => Some(Correction::Uncorrected),
            "fdr" => Some(Correction::Fdr),
            "bonferroni" => Some(Correction::Bonferroni),
            _ => None,
        }
    }
}

fn bend_scores(x: &[f64], beta: f64) -> Option<Vec<f64>> {
    let n = x.len();
    let m = median(x);
    let mut w: Vec<f64> = x.iter().map(|v| (v - m).abs()).collect();
    w.sort_by(f64::total_cmp);
    let k = (((1.0 - beta) * n as f64).floor() as usize).clamp(1, n);
    let omega = w[k - 1];
    if omega <= 0.0 {
        return None;
    }
    let psi: Vec<f64> = x.iter().map(|v| (v - m) / omega).collect();
    let i1 = psi.iter().filter(|p| **p < -1.0).count();
    let i2 = psi.iter().filter(|p| **p > 1.0).count();
    let s: f64 = x
        .iter()
        .zip(&psi)
        .filter(|(_, p)| **p >= -1.0 && **p <= 1.0)
        .map(|(v, _)| *v)
        .sum();
    let denom = n as f64 - i1 as f64 - i2 as f64;
    if denom <= 0.0 {
        return None;
    }
    let phi = (omega * (i2 as f64 - i1 as f64) + s) / denom;
    Some(
        x.iter()
            .map(|v| ((v - phi) / omega).clamp(-1.0, 1.0))
            .collect(),
    )
}

/// Percentage-bend correlation (Wilcox 1994), `beta` 0.2 by default; NaN below 3 pairs or for
/// degenerate input.
pub fn bend_corr(x: &[f64], y: &[f64], beta: f64) -> f64 {
    bend_corr_checked(x, y, beta).unwrap_or(f64::NAN)
}

fn bend_corr_checked(x: &[f64], y: &[f64], beta: f64) -> Option<f64> {
    if x.len() != y.len() || x.len() < 3 {
        return None;
    }
    let a = bend_scores(x, beta)?;
    let b = bend_scores(y, beta)?;
    let sab: f64 = a.iter().zip(&b).map(|(p, q)| p * q).sum();
    let saa: f64 = a.iter().map(|p| p * p).sum();
    let sbb: f64 = b.iter().map(|q| q * q).sum();
    if saa <= 0.0 || sbb <= 0.0 {
        return None;
    }
    Some((sab / (saa * sbb).sqrt()).clamp(-1.0, 1.0))
}

/// Correlation of the given kind with its p; `(NaN, NaN)` below 3 pairs or for constant input.
pub fn correlate(kind: CorrKind, x: &[f64], y: &[f64]) -> (f64, f64) {
    let r = match kind {
        CorrKind::Spearman => spearman(x, y),
        CorrKind::Pearson => pearson(x, y),
        CorrKind::Bend => bend_corr(x, y, 0.2),
    };
    if r.is_nan() || x.len() < 3 {
        (f64::NAN, f64::NAN)
    } else {
        (r, correlation_p(r, x.len()))
    }
}

/// Weighted least-squares t of the slope of `y` on the indicator `g` (with intercept), with
/// `df` degrees of freedom: `(t, p)`.
pub fn weighted_group_t(y: &[f64], g: &[f64], w: &[f64], df: f64) -> (f64, f64) {
    let sw: f64 = w.iter().sum();
    let swg: f64 = w.iter().zip(g).map(|(w, g)| w * g).sum();
    let swy: f64 = w.iter().zip(y).map(|(w, y)| w * y).sum();
    let swgg: f64 = w.iter().zip(g).map(|(w, g)| w * g * g).sum();
    let swgy: f64 = w.iter().zip(g).zip(y).map(|((w, g), y)| w * g * y).sum();
    let det = sw * swgg - swg * swg;
    if det.abs() < 1e-12 || df <= 0.0 {
        return (f64::NAN, f64::NAN);
    }
    let b0 = (swgg * swy - swg * swgy) / det;
    let b1 = (sw * swgy - swg * swy) / det;
    let rss: f64 = (0..y.len())
        .map(|i| w[i] * (y[i] - b0 - b1 * g[i]).powi(2))
        .sum();
    let s2 = rss / df;
    let var_b1 = s2 * sw / det;
    if var_b1 <= 0.0 {
        return (f64::NAN, f64::NAN);
    }
    let t = b1 / var_b1.sqrt();
    (t, t_two_sided_p(t, df))
}

/// Adjusted p values: Bonferroni `p * m`; FDR `p * m / rank` without the monotone step-up
/// (`specs/14` §7.4); NaN stays NaN.
pub fn adjust_p(p: &[f64], method: Correction) -> Vec<f64> {
    let finite: Vec<usize> = (0..p.len()).filter(|i| p[*i].is_finite()).collect();
    let m = finite.len() as f64;
    let mut out = p.to_vec();
    match method {
        Correction::Uncorrected => {}
        Correction::Bonferroni => {
            for i in &finite {
                out[*i] = (p[*i] * m).min(1.0);
            }
        }
        Correction::Fdr => {
            let mut order = finite.clone();
            order.sort_by(|a, b| p[*a].total_cmp(&p[*b]));
            let mut i = 0;
            while i < order.len() {
                let mut j = i;
                while j + 1 < order.len() && p[order[j + 1]] == p[order[i]] {
                    j += 1;
                }
                for k in i..=j {
                    out[order[k]] = (p[order[k]] * m / (i + 1) as f64).min(1.0);
                }
                i = j + 1;
            }
        }
    }
    out
}

/// A small deterministic generator (xoshiro256** seeded through splitmix64).
#[derive(Debug, Clone)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    /// A generator for `seed`.
    pub fn new(seed: u64) -> Rng {
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^ (x >> 31)
        };
        Rng {
            s: [next(), next(), next(), next()],
        }
    }

    /// A generator for stream `k` of `seed`, independent of the order of use.
    pub fn stream(seed: u64, k: u64) -> Rng {
        Rng::new(seed ^ k.wrapping_mul(0xD6E8_FEB8_6659_FD93).rotate_left(17))
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// A uniform value in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform integer in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// A standard normal value (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-300);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Fisher-Yates shuffle.
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn normal_functions_match_known_values() {
        assert!(close(norm_cdf(1.96), 0.975_002_1, 1e-6));
        assert!(close(norm_inv_cdf(0.975), 1.959_964, 1e-6));
        assert!(close(norm_inv_cdf(0.5), 0.0, 1e-12));
    }

    #[test]
    fn t_and_chi2_tails_match_known_values() {
        assert!(close(t_two_sided_p(2.228_139, 10.0), 0.05, 1e-5));
        assert!(close(chi2_1_sf(3.841_459), 0.05, 1e-5));
    }

    #[test]
    fn fdr_reference_form() {
        let q = fdr_reference(&[0.01, 0.04, 0.03]);
        assert!(close(q[0], 0.03, 1e-12) && close(q[1], 0.04, 1e-12) && close(q[2], 0.045, 1e-12));
    }

    #[test]
    fn one_sample_t_hand_value() {
        let r = t_test_one_sample(&[10.0, 20.0, 30.0], 0.0);
        assert!(close(r.stat, 20.0 / (10.0 / 3f64.sqrt()), 1e-12));
    }

    #[test]
    fn exact_binomial_is_two_sided() {
        assert!(close(binomial_two_sided_p(5, 10, 0.5), 1.0, 1e-9));
        assert!(close(binomial_two_sided_p(9, 10, 0.5), 22.0 / 1024.0, 1e-9));
    }

    #[test]
    fn signed_rank_exact_small_sample() {
        let r = wilcoxon_signed_rank(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!(close(r.p, 0.0625, 1e-12));
    }

    #[test]
    fn spearman_of_monotone_is_one() {
        assert!(close(
            spearman(&[1.0, 2.0, 3.0, 4.0], &[1.0, 8.0, 27.0, 64.0]),
            1.0,
            1e-12
        ));
    }

    #[test]
    fn residuals_remove_the_covariate() {
        let x = vec![vec![1.0, 2.0, 3.0, 4.0]];
        let y = [2.0, 4.0, 6.0, 8.0];
        assert!(ols_residuals(&y, &x).iter().all(|v| v.abs() < 1e-9));
    }

    #[test]
    fn van_albada_is_symmetric() {
        let z = van_albada(&[3.0, 1.0, 2.0]);
        assert!(close(z[1], -z[0], 1e-9) && close(z[2], 0.0, 1e-9));
    }

    #[test]
    fn p_values_match_tables() {
        assert!((t_two_sided_p(2.228, 10.0) - 0.05).abs() < 1e-3);
        assert!((chi2_1_sf(3.841) - 0.05).abs() < 1e-3);
        assert!((norm_cdf(1.96) - 0.975).abs() < 1e-4);
    }

    #[test]
    fn correlations_of_monotone_data() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [1.0, 4.0, 9.0, 16.0, 25.0];
        assert!((spearman(&x, &y) - 1.0).abs() < 1e-12);
        assert!(pearson(&x, &y) < 1.0);
        assert!(bend_corr(&x, &y, 0.2) > 0.9);
        assert!(bend_corr(&x[..2], &y[..2], 0.2).is_nan());
    }

    #[test]
    fn adjust_p_fdr_has_no_monotone_step() {
        let p = adjust_p(&[0.01, 0.04, 0.03], Correction::Fdr);
        assert!((p[0] - 0.03).abs() < 1e-12);
        assert!((p[1] - 0.04).abs() < 1e-12);
        assert!((p[2] - 0.045).abs() < 1e-12);
        let nan = adjust_p(&[0.01, f64::NAN], Correction::Bonferroni);
        assert!((nan[0] - 0.01).abs() < 1e-12 && nan[1].is_nan());
    }

    #[test]
    fn covariate_residuals_remove_covariate() {
        let c = vec![1.0, 2.0, 3.0, 4.0];
        let y = vec![3.0, 5.0, 7.0, 9.0];
        let r = covariate_residuals(&y, &[c]);
        assert!(r.iter().all(|v| v.abs() < 1e-9));
        let short = covariate_residuals(&[1.0, 2.0], &[vec![1.0, 2.0]]);
        assert!(short.iter().all(|v| v.is_nan()));
    }

    #[test]
    fn linear_regression_recovers_line() {
        let (b0, b1) =
            linear_regression(&[1.0, 2.0, 3.0, f64::NAN], &[3.0, 5.0, 7.0, 1.0]).unwrap();
        assert!(close(b0, 1.0, 1e-12) && close(b1, 2.0, 1e-12));
        assert!(linear_regression(&[1.0, 1.0], &[1.0, 2.0]).is_none());
    }

    #[test]
    fn fisher_z_and_nan() {
        assert!(close(fisher_z(0.5), 0.549_306_144_334_054_9, 1e-12));
        assert!(fisher_z(f64::NAN).is_nan());
        assert!(fisher_z(1.0).is_finite());
        let (r, _) = correlate(CorrKind::Pearson, &[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]);
        assert!(r.is_nan());
    }

    #[test]
    fn hand_computed_t() {
        let r = t_test_one_sample(&[10.0, 20.0, 30.0], 0.0);
        assert!(close(r.stat, 3.464_101_6, 1e-6));
    }

    #[test]
    fn rank_tests_detect_shift() {
        let a = [5.0, 6.0, 7.0, 8.0, 9.0];
        let b = [1.0, 2.0, 3.0, 4.0, 5.5];
        let r = wilcoxon_rank_sum(&a, &b);
        assert!(r.stat > 27.5 && r.p < 0.05);
        let r = wilcoxon_signed_rank(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        assert!(r.stat > 18.0 && r.p < 0.05);
    }

    #[test]
    fn weighted_group_t_matches_two_sample_t() {
        let y = [1.0, 2.0, 3.0, 4.0, 5.0, 7.0];
        let g = [1.0, 1.0, 1.0, 0.0, 0.0, 0.0];
        let w = [1.0; 6];
        let (t, p) = weighted_group_t(&y, &g, &w, 4.0);
        let r = t_test_two_sample(&y[..3], &y[3..]);
        assert!(close(t, r.stat, 1e-9) && close(p, r.p, 1e-9));
    }

    #[test]
    fn rng_shuffle_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let mut va: Vec<u32> = (0..10).collect();
        let mut vb = va.clone();
        a.shuffle(&mut va);
        b.shuffle(&mut vb);
        assert_eq!(va, vb);
    }

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(a.next_u64(), b.next_u64());
    }
}
