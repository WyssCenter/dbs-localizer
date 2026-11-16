# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""benchmarks.accuracy: overlap, distance, landmark, Jacobian and paired statistics on tiny
synthetic arrays (no GPU, no data, no network)."""

from __future__ import annotations

import math

import numpy as np
import pytest

from benchmarks.accuracy import landmarks, metrics, stats

pytest.importorskip("scipy")


def _rot(deg: float) -> np.ndarray:
    a = np.deg2rad(deg)
    return np.array([[np.cos(a), -np.sin(a), 0.0], [np.sin(a), np.cos(a), 0.0], [0.0, 0.0, 1.0]])


def _box(shape, lo, hi) -> np.ndarray:
    m = np.zeros(shape, dtype=bool)
    m[lo[0] : hi[0], lo[1] : hi[1], lo[2] : hi[2]] = True
    return m


def test_dice_and_jaccard():
    a = _box((10, 10, 10), (0, 0, 0), (4, 5, 5))
    b = _box((10, 10, 10), (2, 0, 0), (6, 5, 5))
    assert metrics.dice(a, b) == pytest.approx(0.5)
    assert metrics.jaccard(a, b) == pytest.approx(1 / 3)
    assert metrics.dice(a, a) == 1.0
    assert math.isnan(metrics.dice(np.zeros(3, bool), np.zeros(3, bool)))
    assert math.isnan(metrics.jaccard(np.zeros(3, bool), np.zeros(3, bool)))


def test_surface_distances_of_single_voxels_use_the_spacing():
    a = np.zeros((8, 8, 12), bool)
    b = np.zeros_like(a)
    a[5, 5, 5] = True
    b[5, 5, 8] = True
    d_ab, d_ba = metrics.surface_distances(a, b, (1.0, 1.0, 2.0))
    np.testing.assert_allclose(d_ab, [6.0])
    np.testing.assert_allclose(d_ba, [6.0])
    assert metrics.hd95(d_ab, d_ba) == pytest.approx(6.0)
    assert metrics.assd(d_ab, d_ba) == pytest.approx(6.0)


def test_identical_masks_have_zero_distance_and_shifted_ones_do_not():
    a = _box((20, 20, 20), (4, 4, 4), (14, 14, 14))
    d_ab, d_ba = metrics.surface_distances(a, a, (1.0, 1.0, 1.0))
    assert d_ab.max() == 0 and d_ba.max() == 0
    b = np.roll(a, 3, axis=0)
    d_ab, d_ba = metrics.surface_distances(a, b, (2.0, 1.0, 1.0))
    assert max(d_ab.max(), d_ba.max()) == pytest.approx(6.0)
    assert 0 < metrics.assd(d_ab, d_ba) < metrics.hd95(d_ab, d_ba) <= 6.0


def test_label_metrics_skip_absent_labels_and_score_missing_ones():
    ref = np.zeros((10, 10, 10), np.int32)
    warped = np.zeros_like(ref)
    ref[2:5, 2:5, 2:5] = 10
    warped[2:5, 2:5, 2:5] = 10
    ref[6:9, 6:9, 6:9] = 11
    rows = metrics.label_metrics(ref, warped, [10, 11, 12], (1.0, 1.0, 1.0))
    by = {r["label"]: r for r in rows}
    assert set(by) == {10, 11}
    assert by[10]["dice"] == 1.0 and by[10]["hd95_mm"] == 0.0
    assert by[11]["dice"] == 0.0 and math.isnan(by[11]["hd95_mm"])
    fast = metrics.label_metrics(ref, warped, [10], (1.0, 1.0, 1.0), surface=False)
    assert math.isnan(fast[0]["hd95_mm"])
    with pytest.raises(ValueError, match="shape"):
        metrics.label_metrics(ref, warped[:5], [10], (1.0, 1.0, 1.0))


def _grid(shape, spacing, origin, direction) -> np.ndarray:
    m = metrics.index_to_physical_matrix(spacing, direction)
    idx = np.indices(shape, dtype=np.float64).reshape(3, -1)
    return (m @ idx).T.reshape(*shape, 3) + np.asarray(origin)


def test_physical_index_round_trip():
    spacing, origin, direction = (1.0, 2.0, 3.0), (5.0, -4.0, 2.0), _rot(20)
    pts = np.array([[1.0, 2.0, 3.0], [0.0, -7.5, 4.25]])
    idx = metrics.physical_to_index(pts, origin, spacing, direction)
    back = idx @ metrics.index_to_physical_matrix(spacing, direction).T + origin
    np.testing.assert_allclose(back, pts)


def test_jacobian_of_a_linear_field_is_its_determinant_on_an_oblique_grid():
    spacing, origin, direction = (1.0, 2.0, 1.5), (3.0, -2.0, 1.0), _rot(30)
    a = np.array([[1.1, 0.05, 0.0], [-0.02, 0.9, 0.1], [0.0, 0.03, 1.2]])
    x = _grid((9, 7, 8), spacing, origin, direction)
    field = x @ (a - np.eye(3)).T
    det = metrics.jacobian_determinant(field, spacing, direction, slab=3)
    np.testing.assert_allclose(det, np.linalg.det(a), rtol=1e-5)
    assert det.dtype == np.float32
    summary = metrics.jacobian_summary(det)
    assert summary["jac_nonpositive_frac"] == 0.0
    folded = metrics.jacobian_determinant(
        x @ (np.diag([-1.0, 1, 1]) - np.eye(3)).T, spacing, direction
    )
    assert metrics.jacobian_summary(folded)["jac_nonpositive_frac"] == 1.0
    zero = metrics.jacobian_determinant(np.zeros((4, 4, 4, 3)), (1, 1, 1), np.eye(3))
    np.testing.assert_allclose(zero, 1.0)


def test_mask_limits_the_jacobian_summary():
    det = np.ones((4, 4, 4), np.float32)
    det[0] = -1.0
    mask = np.zeros_like(det, dtype=bool)
    mask[2:] = True
    assert metrics.jacobian_summary(det, mask)["jac_nonpositive_frac"] == 0.0
    assert metrics.jacobian_summary(det)["jac_nonpositive_frac"] == pytest.approx(0.25)
    assert math.isnan(metrics.jacobian_summary(det, np.zeros_like(mask))["jac_min"])


def test_map_points_with_constant_and_linear_fields():
    spacing, origin, direction = (2.0, 1.0, 1.0), (-4.0, 3.0, 0.0), _rot(-15)
    x = _grid((10, 12, 9), spacing, origin, direction)
    shift = np.array([1.5, -2.0, 0.25])
    const = np.broadcast_to(shift, x.shape).copy()
    pts = x[[2, 5, 7], [3, 6, 8], [1, 4, 6]] + 0.3
    np.testing.assert_allclose(
        metrics.map_points(const, pts, origin, spacing, direction), pts + shift
    )
    a = np.array([[0.02, 0.0, 0.01], [0.0, -0.03, 0.0], [0.01, 0.0, 0.05]])
    linear = x @ a.T
    np.testing.assert_allclose(
        metrics.map_points(linear, pts, origin, spacing, direction), pts + pts @ a.T, atol=1e-9
    )
    err = metrics.landmark_errors(pts + [3.0, 4.0, 0.0], pts)
    np.testing.assert_allclose(err, 5.0)


def test_field_difference():
    a = np.zeros((3, 3, 3, 3))
    b = np.zeros_like(a)
    b[..., 0] = 3.0
    b[..., 1] = 4.0
    d = metrics.field_difference(a, b)
    assert d["field_diff_rms_mm"] == pytest.approx(5.0)
    assert d["field_diff_max_mm"] == pytest.approx(5.0)
    mask = np.zeros((3, 3, 3), bool)
    assert math.isnan(metrics.field_difference(a, b, mask)["field_diff_rms_mm"])


def test_paired_summary():
    a = [0.80, 0.82, 0.78, 0.85, math.nan]
    b = [0.79, 0.80, 0.78, 0.81, 0.70]
    s = stats.paired_summary(a, b, n_boot=2000)
    assert s["n"] == 4
    assert s["mean_diff"] == pytest.approx(np.mean([0.01, 0.02, 0.0, 0.04]))
    assert s["ci_low"] <= s["mean_diff"] <= s["ci_high"]
    assert stats.paired_summary(a, b, n_boot=2000) == s
    assert 0.0 < s["wilcoxon_p"] <= 1.0
    assert stats.wilcoxon_p([0.0, 0.0, 0.0]) == 1.0
    assert math.isnan(stats.wilcoxon_p([1.0]))
    with pytest.raises(ValueError):
        stats.paired_summary([1.0], [1.0, 2.0])


def test_fcsv_ras_points_become_lps():
    text = (
        "# Markups fiducial file version = 4.6\n# CoordinateSystem = 0\n"
        "# columns = id,x,y,z,ow,ox,oy,oz,vis,sel,lock,label,desc,associatedNodeID\n"
        "n1,1.5,-2,3,0,0,0,1,1,1,1,1,AC,vol\nn2,4,5,-6,0,0,0,1,1,1,1,2,PC,vol\n"
    )
    pts = landmarks.parse_fcsv(text)
    assert pts["1"][0] == "AC"
    np.testing.assert_allclose(pts["1"][1], [-1.5, 2.0, 3.0])
    lps = landmarks.parse_fcsv(text.replace("CoordinateSystem = 0", "CoordinateSystem = LPS"))
    np.testing.assert_allclose(lps["2"][1], [4.0, 5.0, -6.0])
    labels, descs, p, q = landmarks.matched(pts, {"2": lps["2"]})
    assert labels == ["2"] and descs == ["PC"] and p.shape == q.shape == (1, 3)
    with pytest.raises(ValueError, match="duplicate"):
        landmarks.parse_fcsv(text + "n3,0,0,0,0,0,0,1,1,1,1,1,AC,vol\n")
    with pytest.raises(ValueError, match="coordinate system"):
        landmarks.parse_fcsv(text.replace("CoordinateSystem = 0", "CoordinateSystem = 2"))
