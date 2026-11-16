# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Paired comparison of two engines over the same registration pairs."""

from __future__ import annotations

import math
from collections.abc import Sequence
from typing import Any

import numpy as np


def bootstrap_mean_ci(
    values: Sequence[float], *, n_boot: int = 10000, level: float = 0.95, seed: int = 0
) -> tuple[float, float]:
    """Percentile bootstrap confidence interval of the mean (fixed seed, so it repeats)."""
    v = np.asarray(values, dtype=np.float64)
    if v.size == 0:
        return (math.nan, math.nan)
    if v.size == 1:
        return (float(v[0]), float(v[0]))
    rng = np.random.default_rng(seed)
    means = v[rng.integers(0, v.size, size=(n_boot, v.size))].mean(axis=1)
    tail = (1.0 - level) / 2.0 * 100.0
    lo, hi = np.percentile(means, [tail, 100.0 - tail])
    return (float(lo), float(hi))


def wilcoxon_p(differences: Sequence[float]) -> float:
    """Two-sided Wilcoxon signed-rank p-value of paired differences (NaN without SciPy or with
    fewer than two pairs; 1.0 when every difference is zero)."""
    d = np.asarray(differences, dtype=np.float64)
    if d.size < 2:
        return math.nan
    if not np.any(d != 0):
        return 1.0
    try:
        from scipy import stats
    except ImportError:
        return math.nan
    return float(stats.wilcoxon(d).pvalue)


def paired_summary(
    a: Sequence[float], b: Sequence[float], *, n_boot: int = 10000, seed: int = 0
) -> dict[str, Any]:
    """Summary of paired values ``a`` (FastWarp) and ``b`` (ANTs): means, SDs, the mean and
    median of ``a - b`` with a 95 % bootstrap CI, and the Wilcoxon signed-rank p-value.

    Pairs where either value is NaN are dropped; ``n`` is the number kept.
    """
    x, y = np.asarray(a, dtype=np.float64), np.asarray(b, dtype=np.float64)
    if x.shape != y.shape:
        raise ValueError(f"paired arrays differ in shape: {x.shape} vs {y.shape}")
    keep = ~(np.isnan(x) | np.isnan(y))
    x, y = x[keep], y[keep]
    d = x - y
    lo, hi = bootstrap_mean_ci(d, n_boot=n_boot, seed=seed)

    def sd(v: np.ndarray) -> float:
        return float(v.std(ddof=1)) if v.size > 1 else math.nan

    return {
        "n": int(d.size),
        "mean_a": float(x.mean()) if x.size else math.nan,
        "sd_a": sd(x),
        "mean_b": float(y.mean()) if y.size else math.nan,
        "sd_b": sd(y),
        "mean_diff": float(d.mean()) if d.size else math.nan,
        "median_diff": float(np.median(d)) if d.size else math.nan,
        "ci_low": lo,
        "ci_high": hi,
        "wilcoxon_p": wilcoxon_p(d),
    }
