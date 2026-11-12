# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""register_syn(recipe=...): the schedule as data, checked before any work."""

from __future__ import annotations

import copy
import hashlib
from pathlib import Path
from types import MappingProxyType

import fastwarp as g
import pytest
from test_register import SHIFT_MM, SPACING, needs_gpu, phantom


def _digest(prefix: Path) -> str:
    h = hashlib.sha256()
    for s in ("Composite.h5", "InverseComposite.h5", "0GenericAffine.mat"):
        h.update(Path(f"{prefix}{s}").read_bytes())
    return h.hexdigest()


def test_cohort_recipe_has_every_field():
    one, two = g.cohort_recipe(1), g.cohort_recipe(2)
    assert set(one) == {"rigid", "affine", "syn"}
    assert one["syn"]["channel_weights"] == [1.0]
    assert two["syn"]["channel_weights"] == [0.5, 0.5]
    assert one["rigid"]["iterations"] == [1000, 500, 250, 100]
    assert one["syn"]["iterations"] == [100, 70, 50, 20]
    assert one["syn"]["cc_radius"] == 4 and one["rigid"]["bins"] == 32
    with pytest.raises(ValueError):
        g.cohort_recipe(3)


@needs_gpu
def test_the_explicit_cohort_recipe_is_the_default_bit_for_bit(tmp_path):
    a, b = tmp_path / "default_", tmp_path / "explicit_"
    info = g.register_syn(phantom(), phantom(SHIFT_MM), a, iso_mm=SPACING)
    g.register_syn(phantom(), phantom(SHIFT_MM), b, iso_mm=SPACING, recipe=g.cohort_recipe(1))
    assert _digest(a) == _digest(b)
    assert info["recipe"] == g.cohort_recipe(1)


@needs_gpu
def test_an_override_runs_and_is_reported(tmp_path):
    recipe = {
        "syn": {
            "iterations": [30, 10, 5],
            "shrink_factors": [4, 2, 1],
            "smoothing_sigmas": [2.0, 1.0, 0.0],
            "cc_radius": 2,
        },
        "affine": MappingProxyType({"iterations": [200, 100, 50, 20]}),
    }
    info = g.register_syn(
        phantom(), phantom(SHIFT_MM), tmp_path / "r_", iso_mm=SPACING, recipe=recipe
    )
    assert [lvl[0] for lvl in info["levels"]] == [0, 1, 2]
    assert all(lvl[2] <= n for lvl, n in zip(info["levels"], [30, 10, 5], strict=True))
    ran = info["recipe"]
    assert ran["syn"]["cc_radius"] == 2 and ran["syn"]["iterations"] == [30, 10, 5]
    assert ran["affine"]["iterations"] == [200, 100, 50, 20]
    assert ran["rigid"] == g.cohort_recipe(1)["rigid"]
    # Still registers: the affine recovers the shift.
    assert abs(info["affine_parameters"][9] - SHIFT_MM[0]) < 0.3


def _bad(**stages):
    r = copy.deepcopy(g.cohort_recipe(1))
    for stage, fields in stages.items():
        r[stage].update(fields)
    return r


@pytest.mark.parametrize(
    ("recipe", "match"),
    [
        ({"rigid": {}, "warp": {}}, "unknown key 'warp'"),
        ({"syn": {"iteration": [1]}}, "unknown key 'iteration'"),
        ({"syn": "SyN[0.1,3,0]"}, "must be a dict"),
        (_bad(syn={"iterations": "100x70"}), "got a string"),
        (_bad(syn={"iterations": [100, -1, 5, 2]}), r"\['iterations'\]"),
        (_bad(syn={"iterations": [100, 70]}), "one entry per level"),
        (_bad(rigid={"shrink_factors": [8, 4, 2, 0]}), "one entry per level"),
        (_bad(affine={"bins": 64}), "32 bins"),
        (_bad(syn={"cc_radius": 9}), "1 to 8"),
        (_bad(syn={"cc_radius": 0}), "CC radius"),
        (_bad(syn={"channel_weights": [0.5, 0.5]}), "2 weights for 1 channel"),
        (_bad(syn={"sigma_units": "px"}), "'vox' or 'mm'"),
        (_bad(rigid={"sampling_percentage": 1.5}), "sampling percentage"),
        (_bad(syn={"gradient_step": 0.0}), "gradient step"),
        (_bad(syn={"smoothing_sigmas": [3.0, 2.0, float("nan"), 0.0]}), "smoothing sigmas"),
        (_bad(affine={"convergence_window": 1}), "window"),
    ],
)
def test_a_bad_recipe_is_refused_before_any_work(tmp_path, monkeypatch, recipe, match):
    # The GPU is never opened: the check runs before the engine starts.
    monkeypatch.setenv("FASTWARP_GPU_ADAPTER", "no such adapter")
    with pytest.raises(ValueError, match=match):
        g.register_syn(phantom(), phantom(), tmp_path / "x_", iso_mm=SPACING, recipe=recipe)
    assert not list(tmp_path.iterdir())
