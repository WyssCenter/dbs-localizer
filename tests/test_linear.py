# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""LinearSession: affine fits and per-fit masks (the rigid fits are in test_rigid.py)."""

from __future__ import annotations

import fastwarp as g
import numpy as np
import pytest
from test_register import SPACING, needs_gpu, phantom

ants = pytest.importorskip("ants")

#: Moving = fixed seen through this affine: rotated 3° about z, scaled per axis, shifted.
_A = np.deg2rad(3.0)
MATRIX = np.array(
    [[np.cos(_A), -np.sin(_A), 0.0], [np.sin(_A), np.cos(_A), 0.0], [0.0, 0.0, 1.0]]
) @ np.diag([1.04, 0.97, 1.02])
SHIFT = np.array([3.0, -2.0, 2.5])
#: Scaling alone for the comparison with ANTs: from the centres of mass, a rotation traded
#: against scaling leaves this phantom's affine poorly determined, and ANTs' own fits of the
#: rotated pose spread over 1 mm with the seed (the GPU's land closer to the truth).
SCALING = np.diag([1.06, 0.95, 1.03])
#: ANTsPy's "Affine" preset (Affine[0.25], 2100x1200x1200x10, 6x4x2x1, 3x2x1x0 vox).
ANTSPY_AFFINE = {
    "gradient_step": 0.25,
    "iterations": [2100, 1200, 1200, 10],
    "shrink_factors": [6, 4, 2, 1],
    "smoothing_sigmas": [3.0, 2.0, 1.0, 0.0],
    "sigma_units": "vox",
}


def _image():
    vol, sp, origin, _ = phantom()
    return ants.from_numpy(vol.astype("float32"), spacing=sp, origin=origin)


def _centre(img) -> np.ndarray:
    return np.asarray(img.origin) + (np.asarray(img.shape) - 1) * SPACING / 2


def _seen_through(fixed, matrix, shift):
    """The moving image m with m(T(p)) = fixed(p) for T = (matrix, shift) about the centre."""
    tx = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=matrix,
        translation=tuple(shift),
        center=tuple(_centre(fixed)),
    )
    return tx.invert().apply_to_image(fixed, reference=fixed, interpolation="linear"), tx


def _corners(img) -> np.ndarray:
    lo, hi = np.asarray(img.origin), _centre(img) * 2 - np.asarray(img.origin)
    inner = lo + 0.25 * (hi - lo), lo + 0.75 * (hi - lo)
    return np.array(
        [[inner[i][0], inner[j][1], inner[k][2]] for i in (0, 1) for j in (0, 1) for k in (0, 1)]
    )


def _apply(params, center, pts) -> np.ndarray:
    m, t, c = np.asarray(params[:9]).reshape(3, 3), np.asarray(params[9:]), np.asarray(center)
    return (pts - c) @ m.T + c + t


def _gap(info, tx, pts) -> float:
    """Largest distance between where the fit and the true affine send ``pts`` (mm)."""
    true = np.array([tx.apply_to_point(tuple(p)) for p in pts])
    return float(
        np.max(np.linalg.norm(_apply(info["parameters"], info["center"], pts) - true, axis=1))
    )


@pytest.fixture(scope="module")
def pair():
    fixed = _image()
    moving, tx = _seen_through(fixed, MATRIX, SHIFT)
    return fixed, moving, tx


@needs_gpu
def test_a_rigid_then_affine_fit_recovers_scaling(pair, tmp_path):
    fixed, moving, tx = pair
    with g.linear_session(fixed, moving) as s:
        rigid = s.register()
        affine = s.register(
            transform="affine",
            initial=(rigid["parameters"], rigid["center"]),
            out_prefix=tmp_path / "a_",
        )
    assert rigid["transform"] == "rigid" and affine["transform"] == "affine"
    np.testing.assert_allclose(
        np.asarray(affine["parameters"][:9]).reshape(3, 3), MATRIX, atol=0.01
    )
    pts = _corners(fixed)
    # The affine lands 0.09 (Intel), 0.3 (Metal, Radeon) mm from T; ANTs' Affine 1.1-1.3 mm.
    assert _gap(rigid, tx, pts) > 1.0 > 0.5 > _gap(affine, tx, pts)
    # The affine starts as the identity where the rigid fit ended, centred like it.
    np.testing.assert_allclose(affine["initial_parameters"], rigid["parameters"], atol=1e-12)
    np.testing.assert_allclose(affine["center"], rigid["center"], atol=1e-12)
    # ANTs reads the written affine as the fit.
    read = ants.read_transform(str(tmp_path / "a_0GenericAffine.mat"))
    np.testing.assert_allclose(
        [read.apply_to_point(tuple(p)) for p in pts],
        _apply(affine["parameters"], affine["center"], pts),
        atol=1e-4,
    )


@needs_gpu
def test_an_affine_fit_lands_where_ants_does(pair):
    """ANTsPy's "Affine" preset from the centres of mass, on the GPU and in ANTs."""
    fixed = pair[0]
    moving, tx = _seen_through(fixed, SCALING, SHIFT)
    with g.linear_session(fixed, moving) as s:
        gpu = s.register(stage=ANTSPY_AFFINE, transform="affine")
    reg = ants.registration(
        fixed,
        moving,
        type_of_transform="Affine",
        aff_metric="mattes",
        aff_sampling=32,
        aff_random_sampling_rate=0.25,
        random_seed=1,
    )
    ref = ants.read_transform(reg["fwdtransforms"][0])
    pts = _corners(fixed)
    ants_pts = np.array([ref.apply_to_point(tuple(p)) for p in pts])
    gap = np.max(np.linalg.norm(_apply(gpu["parameters"], gpu["center"], pts) - ants_pts, axis=1))
    true = np.array([tx.apply_to_point(tuple(p)) for p in pts])
    ants_truth = float(np.max(np.linalg.norm(ants_pts - true, axis=1)))
    gpu_truth = _gap(gpu, tx, pts)
    seen = f"GPU {gpu_truth:.2f} mm and ANTs {ants_truth:.2f} mm from the truth"
    # ANTs' seeds land 0.3-0.4 mm from the truth here; the GPU 0.17 (Intel) to 0.31 mm (NVIDIA).
    # ITK's threads sum in any order, so a fixed seed still moves ANTs between runs: CI has seen
    # the two fits 0.54 and 0.61 mm apart. This bound is a loose parity check; the GPU's own
    # bound against the truth below is the real test.
    assert gap < 0.75, f"GPU and ANTs affine fits {gap:.2f} mm apart ({seen})"
    assert gpu_truth < 0.5, seen


@needs_gpu
def test_a_masked_fit_follows_the_masked_region_as_ants_does(tmp_path):
    """Moving: the fixed image through T inside a box, through T shifted 4 mm outside it. A fit
    masked to the box follows T there; ANTs' fit with the same fixed mask lands with it."""
    fixed = _image()
    inside, tx = _seen_through(fixed, MATRIX, SHIFT)
    outside, _ = _seen_through(fixed, MATRIX, SHIFT + np.array([4.0, 0.0, 0.0]))
    box = np.zeros(fixed.shape, dtype=bool)
    lo, hi = (
        (np.asarray(fixed.shape) * 0.2).astype(int),
        (np.asarray(fixed.shape) * 0.8).astype(int),
    )
    box[lo[0] : hi[0], lo[1] : hi[1], lo[2] : hi[2]] = True
    moving = fixed.new_image_like(np.where(box, inside.numpy(), outside.numpy()))
    # The fixed points that land in the box, kept 4 voxels away from its faces.
    lo, hi = lo + 4, hi - 4
    roi = np.zeros_like(box)
    roi[lo[0] : hi[0], lo[1] : hi[1], lo[2] : hi[2]] = True
    mask = fixed.new_image_like(roi.astype("float32"))
    pts = np.asarray(fixed.origin) + SPACING * np.array(
        [
            [(lo, hi - 1)[i][0], (lo, hi - 1)[j][1], (lo, hi - 1)[k][2]]
            for i in (0, 1)
            for j in (0, 1)
            for k in (0, 1)
        ]
    )
    # A small ROI is fitted from a near start on fine levels, as a brain-shift refinement is.
    fine = {"iterations": [200, 100], "shrink_factors": [2, 1], "smoothing_sigmas": [1.0, 0.0]}
    with g.linear_session(fixed, moving) as s:
        rigid = s.register(use_mask=False)
        start = (rigid["parameters"], rigid["center"])
        masked = s.register(stage=fine, transform="affine", initial=start, mask=mask)
        unmasked = s.register(stage=fine, transform="affine", initial=start, use_mask=False)
    assert _gap(masked, tx, pts) < 1.0 < 3.0 < _gap(unmasked, tx, pts)
    init = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=np.asarray(start[0][:9]).reshape(3, 3),
        translation=tuple(start[0][9:]),
        center=tuple(start[1]),
    )
    ants.write_transform(init, str(tmp_path / "start.mat"))
    true = np.array([tx.apply_to_point(tuple(p)) for p in pts])
    ants_off = []
    # Single ANTs runs land 0.36-1.4 mm from T here, seed or not (ITK's threads sum in any
    # order); the GPU 0.35 (NVIDIA) to 0.52 mm (Intel). Their median is the fair comparison.
    for _ in range(3):
        reg = ants.registration(
            fixed, moving, type_of_transform="Affine",
            initial_transform=str(tmp_path / "start.mat"), aff_iterations=(200, 100),
            aff_shrink_factors=(2, 1), aff_smoothing_sigmas=(1, 0), aff_metric="mattes",
            aff_sampling=32, aff_random_sampling_rate=0.25, mask=mask, random_seed=1,
        )  # fmt: skip
        ref = ants.read_transform(reg["fwdtransforms"][0])
        theirs = np.array([ref.apply_to_point(tuple(p)) for p in pts])
        ants_off.append(float(np.max(np.linalg.norm(theirs - true, axis=1))))
    assert _gap(masked, tx, pts) < float(np.median(ants_off)) + 0.25, ants_off


@needs_gpu
def test_a_fit_mask_equals_the_session_mask(pair):
    fixed, moving, _ = pair
    mask = fixed.new_image_like((fixed.numpy() > 0.05).astype("float32"))
    with g.linear_session(fixed, moving, fixed_mask=mask) as s:
        a = s.register(transform="affine")
    with g.linear_session(fixed, moving) as s:
        b = s.register(transform="affine", mask=mask)
        c = s.register(transform="affine")
    assert a["parameters"] == b["parameters"] != c["parameters"]


@pytest.mark.parametrize(
    ("kwargs", "match"),
    [
        ({"transform": "similarity"}, "'rigid' or 'affine'"),
        ({"use_mask": False, "mask": "m"}, "use_mask=False"),
        ({"transform": "affine", "stage": {"bins": 64}}, "32 bins"),
    ],
)
@needs_gpu
def test_bad_fit_arguments_are_refused(pair, kwargs, match):
    fixed, moving, _ = pair
    if kwargs.get("mask") == "m":
        kwargs["mask"] = fixed
    with g.linear_session(fixed, moving) as s, pytest.raises(ValueError, match=match):
        s.register(**kwargs)


def test_the_0_3_names_still_work():
    assert g.RigidSession is g.LinearSession and g.rigid_session is g.linear_session
