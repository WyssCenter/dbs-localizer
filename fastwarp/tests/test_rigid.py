# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""register_rigid: a rigid MI registration on the GPU from a given or centre-of-mass start."""

from __future__ import annotations

from pathlib import Path

import fastwarp as g
import numpy as np
import pytest
from fastwarp import _api
from fastwarp._api import register_rigid
from test_register import SPACING, needs_gpu, phantom

ants = pytest.importorskip("ants")

#: Moving = fixed rotated 8° about z and shifted: a pose a rigid fit must recover.
ANGLE = np.deg2rad(8.0)
SHIFT = np.array([5.0, -4.0, 3.0])


def _rot(a: float) -> np.ndarray:
    return np.array([[np.cos(a), -np.sin(a), 0.0], [np.sin(a), np.cos(a), 0.0], [0.0, 0.0, 1.0]])


@pytest.fixture(scope="module")
def pair():
    vol, sp, origin, direction = phantom()
    fixed = ants.from_numpy(vol.astype("float32"), spacing=sp, origin=origin)
    centre = np.asarray(fixed.origin) + (np.asarray(fixed.shape) - 1) * SPACING / 2
    tx = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=_rot(ANGLE),
        translation=tuple(SHIFT),
        center=tuple(centre),
    )
    # The moving image seen through T is the fixed image: moving(T(p)) = fixed(p).
    moving = tx.invert().apply_to_image(fixed, reference=fixed, interpolation="linear")
    return fixed, moving, tx, centre


def _corr(a, b) -> float:
    return float(np.corrcoef(a.numpy().ravel(), b.numpy().ravel())[0, 1])


@needs_gpu
def test_recovers_a_rigid_pose_and_ants_reads_the_transform(pair, tmp_path: Path):
    fixed, moving, tx, _ = pair
    info = register_rigid(fixed, moving, out_prefix=tmp_path / "r_")
    mat = tmp_path / "r_0GenericAffine.mat"
    assert mat.is_file() and info["metric"] < 0
    p = np.asarray(info["parameters"])
    np.testing.assert_allclose(p[:9].reshape(3, 3), _rot(ANGLE), atol=0.01)
    # Where the fit sends the grid centre vs where the true pose sends it.
    c = np.asarray(info["center"])
    true = np.asarray(tx.apply_to_point(tuple(c)))
    fit = p[:9].reshape(3, 3) @ (c - c) + c + p[9:]
    assert np.linalg.norm(fit - true) < 0.5
    warped = ants.apply_transforms(fixed=fixed, moving=moving, transformlist=[str(mat)])
    assert _corr(fixed, warped) > 0.99 > _corr(fixed, moving)


@needs_gpu
def test_an_explicit_start_is_used(pair):
    fixed, moving, tx, centre = pair
    # Starting exactly at the answer ends there; the reported start is the one given.
    start = (np.concatenate([_rot(ANGLE).ravel(), SHIFT]), centre)
    info = register_rigid(fixed, moving, initial=start)
    np.testing.assert_allclose(info["initial_parameters"], start[0], atol=1e-9)
    np.testing.assert_allclose(info["initial_center"], centre, atol=1e-9)
    np.testing.assert_allclose(np.asarray(info["parameters"])[:9], start[0][:9], atol=0.01)


@needs_gpu
def test_an_all_ones_mask_changes_nothing_and_runs_are_deterministic(pair):
    fixed, moving, _, _ = pair
    a = register_rigid(fixed, moving)
    b = register_rigid(fixed, moving, fixed_mask=fixed.new_image_like(np.ones(fixed.shape)))
    c = register_rigid(fixed, moving)
    assert a["parameters"] == b["parameters"] == c["parameters"]


@needs_gpu
def test_a_short_stage_and_winsorizing_run(pair):
    fixed, moving, _, _ = pair
    stage = {"iterations": [50, 20], "shrink_factors": [4, 2], "smoothing_sigmas": [2.0, 1.0]}
    info = register_rigid(fixed, moving, stage=stage, winsorize=(0.005, 0.995))
    assert len(info["iterations"]) == 2 and info["stage"]["iterations"] == [50, 20]
    assert all(0 < n <= m for n, m in zip(info["iterations"], [50, 20], strict=True))


@pytest.mark.parametrize(
    ("kwargs", "match"),
    [
        ({"stage": {"bins": 64}}, "32 bins"),
        ({"stage": {"iteration": [1]}}, "unknown key"),
        ({"stage": {"iterations": [10, 10], "shrink_factors": [2]}}, "one entry per level"),
        ({"winsorize": (0.5, 0.2)}, "winsorize"),
        ({"initial": (np.zeros(11), np.zeros(3))}, None),
        ({"out_prefix": "/nonexistent-dir/x/r_", "_no_mkdir": True}, "does not exist"),
    ],
)
def test_bad_arguments_are_refused_before_any_work(pair, monkeypatch, kwargs, match):
    fixed, moving, _, _ = pair
    monkeypatch.setenv("FASTWARP_GPU_ADAPTER", "no such adapter")
    if kwargs.pop("_no_mkdir", False):
        monkeypatch.setattr(_api, "_prepared_prefix", str)
    with pytest.raises((ValueError, TypeError), match=match):
        register_rigid(fixed, moving, **kwargs)


@needs_gpu
def test_a_session_gives_the_one_shot_result_and_reuses_its_preparation(pair):
    """Kept smoothed images must not change a fit: the same start gives the same affine."""
    fixed, moving, _, _ = pair
    mask = fixed.new_image_like((fixed.numpy() > 0.05).astype("float32"))
    one = register_rigid(fixed, moving, winsorize=(0.005, 0.995))
    one_masked = register_rigid(fixed, moving, winsorize=(0.005, 0.995), fixed_mask=mask)
    with g.linear_session(fixed, moving, fixed_mask=mask, winsorize=(0.005, 0.995)) as s:
        a = s.register(use_mask=False)
        b = s.register(use_mask=True)
        c = s.register(use_mask=False)
        assert s.adapter and len(s.com_initial[0]) == 12
    assert a["parameters"] == c["parameters"] == one["parameters"]
    assert b["parameters"] == one_masked["parameters"]
    with pytest.raises(ValueError, match="closed"):
        s.register()
