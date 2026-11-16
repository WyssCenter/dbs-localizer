# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Overlap, surface-distance, landmark and deformation-field metrics on numpy arrays.

Arrays are indexed ``[i, j, k]`` by ITK voxel index; physical points are in ITK (LPS)
millimetres. A grid is described by ``spacing`` (3 floats) and ``direction`` (3x3, columns are
the axis directions), and an ``origin`` where points are involved.
"""

from __future__ import annotations

import math
from collections.abc import Iterable, Sequence
from typing import Any

import numpy as np


def dice(a: np.ndarray, b: np.ndarray) -> float:
    """Dice overlap ``2|A∩B| / (|A|+|B|)`` of two boolean masks; NaN when both are empty."""
    a, b = np.asarray(a, dtype=bool), np.asarray(b, dtype=bool)
    total = int(a.sum()) + int(b.sum())
    if total == 0:
        return math.nan
    return 2.0 * int(np.logical_and(a, b).sum()) / total


def jaccard(a: np.ndarray, b: np.ndarray) -> float:
    """Jaccard overlap ``|A∩B| / |A∪B|`` of two boolean masks; NaN when both are empty."""
    a, b = np.asarray(a, dtype=bool), np.asarray(b, dtype=bool)
    union = int(np.logical_or(a, b).sum())
    if union == 0:
        return math.nan
    return int(np.logical_and(a, b).sum()) / union


def _bbox(mask: np.ndarray, margin: int = 1) -> tuple[slice, ...]:
    idx = np.nonzero(mask)
    return tuple(
        slice(max(int(i.min()) - margin, 0), min(int(i.max()) + 1 + margin, n))
        for i, n in zip(idx, mask.shape, strict=True)
    )


def _boundary(mask: np.ndarray) -> np.ndarray:
    from scipy import ndimage

    eroded = ndimage.binary_erosion(mask, structure=ndimage.generate_binary_structure(3, 1))
    return mask & ~eroded


def surface_distances(
    a: np.ndarray, b: np.ndarray, spacing: Sequence[float]
) -> tuple[np.ndarray, np.ndarray]:
    """Distances (mm) from each boundary voxel of ``a`` to the boundary of ``b``, and back.

    The boundary of a mask is its voxels with a 6-connected neighbour outside it. Both arrays
    are empty when either mask is empty.
    """
    from scipy import ndimage

    a, b = np.asarray(a, dtype=bool), np.asarray(b, dtype=bool)
    if not a.any() or not b.any():
        return np.empty(0), np.empty(0)
    box = _bbox(a | b)
    a, b = a[box], b[box]
    sa, sb = _boundary(a), _boundary(b)
    sampling = tuple(float(s) for s in spacing)
    to_b = ndimage.distance_transform_edt(~sb, sampling=sampling)
    to_a = ndimage.distance_transform_edt(~sa, sampling=sampling)
    return to_b[sa], to_a[sb]


def hd95(d_ab: np.ndarray, d_ba: np.ndarray) -> float:
    """The 95th-percentile Hausdorff distance: the larger of the two directed 95th percentiles."""
    if d_ab.size == 0 or d_ba.size == 0:
        return math.nan
    return float(max(np.percentile(d_ab, 95), np.percentile(d_ba, 95)))


def assd(d_ab: np.ndarray, d_ba: np.ndarray) -> float:
    """Average symmetric surface distance: the mean of all distances in both directions."""
    n = d_ab.size + d_ba.size
    if d_ab.size == 0 or d_ba.size == 0:
        return math.nan
    return float((d_ab.sum() + d_ba.sum()) / n)


def mask_metrics(
    reference: np.ndarray, warped: np.ndarray, spacing: Sequence[float], *, surface: bool = True
) -> dict[str, Any]:
    """Dice, Jaccard, HD95 and ASSD of the boolean mask ``warped`` against ``reference``.

    ``surface=False`` skips the distances (NaN), which are the slow part.
    """
    a, b = np.asarray(reference, dtype=bool), np.asarray(warped, dtype=bool)
    if a.shape != b.shape:
        raise ValueError(f"masks differ in shape: {a.shape} vs {b.shape}")
    row = {
        "voxels_reference": int(a.sum()),
        "voxels_warped": int(b.sum()),
        "dice": dice(a, b),
        "jaccard": jaccard(a, b),
        "hd95_mm": math.nan,
        "assd_mm": math.nan,
    }
    if surface:
        d_ab, d_ba = surface_distances(a, b, spacing)
        row["hd95_mm"] = hd95(d_ab, d_ba)
        row["assd_mm"] = assd(d_ab, d_ba)
    return row


def label_metrics(
    reference: np.ndarray,
    warped: np.ndarray,
    labels: Iterable[int],
    spacing: Sequence[float],
    *,
    surface: bool = True,
) -> list[dict[str, Any]]:
    """:func:`mask_metrics` per label of two label maps, with a ``label`` key.

    A label missing from both maps is skipped; a label missing from one gets Dice 0 and NaN
    distances.
    """
    reference = np.asarray(reference)
    warped = np.asarray(warped)
    if reference.shape != warped.shape:
        raise ValueError(f"label maps differ in shape: {reference.shape} vs {warped.shape}")
    rows = []
    for label in labels:
        a = reference == label
        b = warped == label
        if not a.any() and not b.any():
            continue
        rows.append({"label": int(label), **mask_metrics(a, b, spacing, surface=surface)})
    return rows


def index_to_physical_matrix(spacing: Sequence[float], direction: Any) -> np.ndarray:
    """The 3x3 matrix ``D diag(spacing)`` mapping a voxel-index step to a physical step."""
    d = np.asarray(direction, dtype=np.float64).reshape(3, 3)
    return d @ np.diag(np.asarray(spacing, dtype=np.float64))


def physical_to_index(points: np.ndarray, origin: Any, spacing: Any, direction: Any) -> np.ndarray:
    """Continuous voxel indices (N x 3) of physical points (N x 3)."""
    m = index_to_physical_matrix(spacing, direction)
    pts = np.atleast_2d(np.asarray(points, dtype=np.float64))
    return np.linalg.solve(m, (pts - np.asarray(origin, dtype=np.float64)).T).T


def sample_field(
    field: np.ndarray, points: np.ndarray, origin: Any, spacing: Any, direction: Any
) -> np.ndarray:
    """Trilinear samples (N x 3) of a displacement field (X x Y x Z x 3) at physical points.

    Points outside the grid take the value of the nearest edge voxel.
    """
    from scipy import ndimage

    idx = physical_to_index(points, origin, spacing, direction).T
    return np.stack(
        [
            ndimage.map_coordinates(field[..., c], idx, order=1, mode="nearest")
            for c in range(field.shape[-1])
        ],
        axis=1,
    )


def map_points(
    field: np.ndarray, points: np.ndarray, origin: Any, spacing: Any, direction: Any
) -> np.ndarray:
    """Points moved by a displacement field: ``p + u(p)``, as ``antsApplyTransformsToPoints``
    moves them with a displacement-field transform."""
    pts = np.atleast_2d(np.asarray(points, dtype=np.float64))
    return pts + sample_field(field, pts, origin, spacing, direction)


def landmark_errors(predicted: np.ndarray, truth: np.ndarray) -> np.ndarray:
    """Euclidean distances (mm) between matching rows of two N x 3 point arrays."""
    p, t = np.atleast_2d(predicted), np.atleast_2d(truth)
    if p.shape != t.shape:
        raise ValueError(f"point arrays differ in shape: {p.shape} vs {t.shape}")
    return np.linalg.norm(p - t, axis=1)


def jacobian_determinant(
    field: np.ndarray, spacing: Sequence[float], direction: Any, *, slab: int = 16
) -> np.ndarray:
    """Determinant of the Jacobian of ``x -> x + u(x)`` for a displacement field ``u``
    (X x Y x Z x 3, vectors in physical mm), by central differences (one-sided at the edges).

    Computed in slabs along the first axis to bound memory; the result is ``float32``.
    """
    field = np.asarray(field)
    if field.ndim != 4 or field.shape[-1] != 3:
        raise ValueError(f"expected an X x Y x Z x 3 field, got shape {field.shape}")
    a = np.linalg.inv(index_to_physical_matrix(spacing, direction))
    n = field.shape[0]
    out = np.empty(field.shape[:3], dtype=np.float32)
    eye = np.eye(3)
    for start in range(0, n, slab):
        stop = min(start + slab, n)
        lo, hi = max(start - 1, 0), min(stop + 1, n)
        part = field[lo:hi].astype(np.float64)
        grads = np.empty((*part.shape[:3], 3, 3))
        for c in range(3):
            for k in range(3):
                if part.shape[k] < 2:
                    grads[..., c, k] = 0.0
                else:
                    grads[..., c, k] = np.gradient(part[..., c], axis=k)
        jac = eye + grads @ a
        out[start:stop] = np.linalg.det(jac[start - lo : start - lo + (stop - start)])
    return out


def jacobian_summary(det: np.ndarray, mask: np.ndarray | None = None) -> dict[str, float]:
    """Fraction of voxels with a determinant ≤ 0 (folding), and the min, 1st percentile, median
    and max of the determinant, inside ``mask`` (everywhere when ``None``)."""
    values = det[mask.astype(bool)] if mask is not None else det.ravel()
    if values.size == 0:
        keys = ("jac_nonpositive_frac", "jac_min", "jac_p01", "jac_median", "jac_max")
        return dict.fromkeys(keys, math.nan)
    return {
        "jac_nonpositive_frac": float(np.mean(values <= 0)),
        "jac_min": float(values.min()),
        "jac_p01": float(np.percentile(values, 1)),
        "jac_median": float(np.median(values)),
        "jac_max": float(values.max()),
    }


def field_difference(a: np.ndarray, b: np.ndarray, mask: np.ndarray | None = None) -> dict:
    """RMS, mean, 95th percentile and max of ``|a - b|`` (mm) of two displacement fields inside
    ``mask`` (everywhere when ``None``)."""
    if a.shape != b.shape:
        raise ValueError(f"fields differ in shape: {a.shape} vs {b.shape}")
    norm = np.linalg.norm(a.astype(np.float64) - b.astype(np.float64), axis=-1)
    values = norm[mask.astype(bool)] if mask is not None else norm.ravel()
    if values.size == 0:
        keys = ("field_diff_rms_mm", "field_diff_mean_mm", "field_diff_p95_mm", "field_diff_max_mm")
        return dict.fromkeys(keys, math.nan)
    return {
        "field_diff_rms_mm": float(np.sqrt(np.mean(values**2))),
        "field_diff_mean_mm": float(values.mean()),
        "field_diff_p95_mm": float(np.percentile(values, 95)),
        "field_diff_max_mm": float(values.max()),
    }
