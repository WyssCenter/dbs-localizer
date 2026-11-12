// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Go / no-go classification of plan section 8.4 and the SyN time extrapolation.

use serde::Serialize;

use crate::caps::Verdict;

/// Summary of repeated measurements.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub samples: Vec<f64>,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub spread_pct: f64,
}

impl Stats {
    /// Builds statistics from samples.
    pub fn from_samples(samples: Vec<f64>) -> Self {
        if samples.is_empty() {
            return Stats::default();
        }
        let mut s = samples.clone();
        s.sort_by(|a, b| a.total_cmp(b));
        let n = s.len();
        let median = if n % 2 == 1 {
            s[n / 2]
        } else {
            0.5 * (s[n / 2 - 1] + s[n / 2])
        };
        let (min, max) = (s[0], s[n - 1]);
        let spread_pct = if median > 0.0 {
            (max - min) / median * 100.0
        } else {
            0.0
        };
        Stats {
            samples,
            median,
            min,
            max,
            spread_pct,
        }
    }

    /// Value at quantile `q` in [0, 1] by nearest rank.
    pub fn quantile(&self, q: f64) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut s = self.samples.clone();
        s.sort_by(|a, b| a.total_cmp(b));
        let i = ((s.len() - 1) as f64 * q).round() as usize;
        s[i.min(s.len() - 1)]
    }
}

/// One row of the section 8.4 table.
#[derive(Debug, Clone, Serialize)]
pub struct GateRow {
    pub gate: String,
    pub measured: String,
    pub go: String,
    pub borderline: String,
    pub no_go: String,
    pub verdict: Verdict,
}

/// Probe 1 gate on streaming bandwidth in GB/s.
pub fn classify_bandwidth(gb_s: f64) -> Verdict {
    if gb_s >= 80.0 {
        Verdict::Go
    } else if gb_s >= 50.0 {
        Verdict::Borderline
    } else {
        Verdict::NoGo
    }
}

/// Probe 2 gate on the CC chain time in ms.
pub fn classify_cc(ms: f64) -> Verdict {
    if ms <= 70.0 {
        Verdict::Go
    } else if ms <= 150.0 {
        Verdict::Borderline
    } else {
        Verdict::NoGo
    }
}

/// Probe 3 gate on the composite gather time in ms.
pub fn classify_gather(ms: f64) -> Verdict {
    if ms <= 15.0 {
        Verdict::Go
    } else if ms <= 40.0 {
        Verdict::Borderline
    } else {
        Verdict::NoGo
    }
}

/// Probe 4 gate: dispatch cost in microseconds (go <= 10, no-go > 50) and readback latency in
/// milliseconds (go <= 1; the 1 to 5 ms borderline band and the no-go above 5 ms are assumptions,
/// the plan leaves them unspecified).
pub fn classify_dispatch(dispatch_us: f64, readback_ms: f64) -> Verdict {
    let d = if dispatch_us <= 10.0 {
        Verdict::Go
    } else if dispatch_us <= 50.0 {
        Verdict::Borderline
    } else {
        Verdict::NoGo
    };
    let r = if readback_ms <= 1.0 {
        Verdict::Go
    } else if readback_ms <= 5.0 {
        Verdict::Borderline
    } else {
        Verdict::NoGo
    };
    d.worst(r)
}

/// SyN iterations per level (coarse to fine) at shrink factors 8, 4, 2, 1.
pub const SYN_LEVELS: [(u32, u32); 4] = [(8, 100), (4, 70), (2, 50), (1, 20)];
/// Dispatch overhead the plan adds for the coarse levels, in seconds.
pub const SYN_COARSE_OVERHEAD_S: f64 = 0.3;

/// Extrapolated 1.0 mm SyN time (plan 8.4 last row).
#[derive(Debug, Clone, Serialize)]
pub struct Extrapolation {
    pub formula: String,
    pub levels: Vec<(u32, u32, f64)>,
    pub finest_equivalent_iterations: f64,
    pub per_iteration_ms: f64,
    pub total_s: f64,
    pub total_s_with_overhead: f64,
    pub verdict: Verdict,
}

/// Computes `N_eq * (2 * P2 + 8 * P3 + 1.5 * P2)` with `N_eq = sum(iterations_l / shrink_l^3)`.
pub fn extrapolate_syn(p2_ms: f64, p3_ms: f64) -> Extrapolation {
    let levels: Vec<(u32, u32, f64)> = SYN_LEVELS
        .iter()
        .map(|&(shrink, iters)| (shrink, iters, 1.0 / f64::from(shrink).powi(3)))
        .collect();
    let n_eq: f64 = levels.iter().map(|&(_, it, f)| f64::from(it) * f).sum();
    let per_iteration_ms = 2.0 * p2_ms + 8.0 * p3_ms + 1.5 * p2_ms;
    let total_s = n_eq * per_iteration_ms * 1e-3;
    let verdict = if total_s <= 30.0 {
        Verdict::Go
    } else if total_s <= 60.0 {
        Verdict::Borderline
    } else {
        Verdict::NoGo
    };
    Extrapolation {
        formula: "T = N_eq * (2*P2 + 8*P3 + 1.5*P2), N_eq = sum_l iterations_l / shrink_l^3 \
                  (levels 100 @ /8, 70 @ /4, 50 @ /2, 20 @ /1); P2, P3 = median probe times"
            .to_string(),
        levels,
        finest_equivalent_iterations: n_eq,
        per_iteration_ms,
        total_s,
        total_s_with_overhead: total_s + SYN_COARSE_OVERHEAD_S,
        verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_median_and_spread() {
        let s = Stats::from_samples(vec![3.0, 1.0, 2.0]);
        assert_eq!(s.median, 2.0);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 3.0);
        assert!((s.spread_pct - 100.0).abs() < 1e-12);
        let e = Stats::from_samples(vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(e.median, 2.5);
    }

    #[test]
    fn gate_thresholds() {
        assert_eq!(classify_bandwidth(80.0), Verdict::Go);
        assert_eq!(classify_bandwidth(79.9), Verdict::Borderline);
        assert_eq!(classify_bandwidth(49.9), Verdict::NoGo);
        assert_eq!(classify_cc(70.0), Verdict::Go);
        assert_eq!(classify_cc(150.1), Verdict::NoGo);
        assert_eq!(classify_gather(15.0), Verdict::Go);
        assert_eq!(classify_gather(40.1), Verdict::NoGo);
        assert_eq!(classify_dispatch(9.0, 0.5), Verdict::Go);
        assert_eq!(classify_dispatch(60.0, 0.5), Verdict::NoGo);
    }

    #[test]
    fn extrapolation_matches_hand_computation() {
        let e = extrapolate_syn(10.0, 5.0);
        let n_eq = 100.0 / 512.0 + 70.0 / 64.0 + 50.0 / 8.0 + 20.0;
        assert!((e.finest_equivalent_iterations - n_eq).abs() < 1e-12);
        assert!((e.per_iteration_ms - (35.0 + 40.0)).abs() < 1e-12);
        assert!((e.total_s - n_eq * 75.0e-3).abs() < 1e-12);
    }
}
