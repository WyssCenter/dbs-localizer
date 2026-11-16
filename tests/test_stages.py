# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""register_syn with a stage-list recipe, configurable winsorizing and the output layouts."""

from __future__ import annotations

import hashlib
from pathlib import Path

import fastwarp as g
import numpy as np
import pytest
from test_register import SHIFT_MM, SPACING, needs_gpu, phantom

LINEAR = {"iterations": [60, 30, 10], "shrink_factors": [4, 2, 1], "smoothing_sigmas": [2, 1, 0]}
SYN = {"iterations": [20, 10], "shrink_factors": [2, 1], "smoothing_sigmas": [1, 0]}
IDENTITY = (1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0)


def _digest(prefix: Path, names=("Composite.h5", "InverseComposite.h5", "0GenericAffine.mat")):
    h = hashlib.sha256()
    for s in names:
        h.update(Path(f"{prefix}{s}").read_bytes())
    return h.hexdigest()


def _names(info) -> list[str]:
    return [Path(f).name for f in info["files"]]


@needs_gpu
def test_the_stage_list_form_of_the_cohort_is_the_default_bit_for_bit(tmp_path, baseline):
    a, b, c = baseline[0], tmp_path / "staged_", tmp_path / "echo_"
    cohort = g.cohort_recipe(1)
    staged = {
        "initial": "com",
        "stages": [
            {"transform": "rigid", **cohort["rigid"]},
            {"transform": "affine", **cohort["affine"]},
            {"transform": "syn", **cohort["syn"]},
        ],
        "winsorize": (0.005, 0.995),
    }
    info = g.register_syn(phantom(), phantom(SHIFT_MM), b, iso_mm=SPACING, recipe=staged)
    assert _digest(a) == _digest(b)
    assert info["recipe"]["initial"] == "com"
    assert [s["transform"] for s in info["recipe"]["stages"]] == ["rigid", "affine", "syn"]
    assert info["recipe"]["winsorize"] == [0.005, 0.995]
    g.register_syn(phantom(), phantom(SHIFT_MM), c, iso_mm=SPACING, recipe=info["recipe"])
    assert _digest(a) == _digest(c)
    assert [s["transform"] for s in info["linear_stages"]] == ["rigid", "affine"]
    assert info["deformable"] and len(info["levels"]) == 4


@needs_gpu
def test_winsorize_none_is_the_plain_rescale_and_is_reported(tmp_path):
    a, b = tmp_path / "default_", tmp_path / "plain_"
    recipe = {"rigid": LINEAR, "affine": LINEAR, "syn": SYN}
    info_a = g.register_syn(phantom(), phantom(SHIFT_MM), a, iso_mm=SPACING, recipe=recipe)
    info_b = g.register_syn(
        phantom(), phantom(SHIFT_MM), b, iso_mm=SPACING, recipe={**recipe, "winsorize": None}
    )
    assert "winsorize" not in info_a["recipe"]
    assert info_b["recipe"]["winsorize"] is None
    assert _digest(a) != _digest(b)
    assert np.allclose(info_b["affine_parameters"][9:], SHIFT_MM, atol=0.5)


@needs_gpu
def test_a_linear_only_chain_writes_linear_composites_and_the_affine(tmp_path):
    ants = pytest.importorskip("ants")
    recipe = {
        "stages": [
            {"transform": "rigid", **LINEAR},
            {"transform": "affine", **LINEAR},
            {"transform": "rigid", **LINEAR},
        ]
    }
    out = tmp_path / "lin_"
    info = g.register_syn(
        phantom(), phantom(SHIFT_MM), out, iso_mm=SPACING, recipe=recipe, write_warped=True
    )
    assert _names(info) == [
        "lin_Composite.h5",
        "lin_InverseComposite.h5",
        "lin_0GenericAffine.mat",
        "lin__Warped.nii.gz",
    ]
    assert not info["deformable"] and info["levels"] == [] and info["syn_s"] == 0.0
    assert [s["transform"] for s in info["linear_stages"]] == ["rigid", "affine", "rigid"]
    assert all(len(s["iterations"]) == 3 for s in info["linear_stages"])
    assert np.allclose(info["affine_parameters"][9:], SHIFT_MM, atol=0.5)
    fixed = ants.from_numpy(phantom()[0].astype("float32"), spacing=(SPACING,) * 3)
    moving = ants.from_numpy(phantom(SHIFT_MM)[0].astype("float32"), spacing=(SPACING,) * 3)
    for tx in ("Composite.h5", "0GenericAffine.mat"):
        mine = g.apply_transforms(moving, fixed, f"{out}{tx}")
        theirs = ants.apply_transforms(fixed=fixed, moving=moving, transformlist=[f"{out}{tx}"])
        assert np.abs(mine.numpy() - theirs.numpy()).max() < 1e-3
    back = g.apply_transforms(fixed, moving, f"{out}InverseComposite.h5")
    inverted = g.apply_transforms(fixed, moving, f"{out}0GenericAffine.mat", invert=[True])
    assert np.abs(back.numpy() - inverted.numpy()).max() < 1e-4
    inverted = g.apply_transforms(fixed, moving, f"{out}Composite.h5", invert=np.array([True]))
    assert np.abs(back.numpy() - inverted.numpy()).max() < 1e-4
    split = g.register_syn(
        phantom(),
        phantom(SHIFT_MM),
        tmp_path / "s_",
        iso_mm=SPACING,
        recipe=recipe,
        outputs="split",
    )
    assert _names(split) == ["s_0GenericAffine.mat"]


@needs_gpu
def test_a_syn_only_chain_from_the_identity_writes_ants_split_files(tmp_path):
    ants = pytest.importorskip("ants")
    out = tmp_path / "syn_"
    info = g.register_syn(
        phantom(),
        phantom((1.0, -1.0, 0.5)),
        out,
        iso_mm=SPACING,
        recipe={"initial": "identity", "stages": [{"transform": "syn", **SYN}]},
        outputs="both",
    )
    assert _names(info) == [
        "syn_Composite.h5",
        "syn_InverseComposite.h5",
        "syn_0GenericAffine.mat",
        "syn_1Warp.nii.gz",
        "syn_1InverseWarp.nii.gz",
    ]
    assert info["linear_stages"] == [] and info["deformable"]
    assert tuple(info["affine_parameters"]) == IDENTITY
    assert tuple(info["initial_parameters"]) == IDENTITY
    fixed = ants.from_numpy(phantom()[0].astype("float32"), spacing=(SPACING,) * 3)
    moving = ants.from_numpy(phantom((1.0, -1.0, 0.5))[0].astype("float32"), spacing=(SPACING,) * 3)
    split = [f"{out}1Warp.nii.gz", f"{out}0GenericAffine.mat"]
    theirs = ants.apply_transforms(fixed=fixed, moving=moving, transformlist=split)
    mine = g.apply_transforms(moving, fixed, f"{out}Composite.h5")
    assert np.abs(mine.numpy() - theirs.numpy()).max() < 1e-3
    inverse = [f"{out}0GenericAffine.mat", f"{out}1InverseWarp.nii.gz"]
    theirs = ants.apply_transforms(
        fixed=moving, moving=fixed, transformlist=inverse, whichtoinvert=[True, False]
    )
    mine = g.apply_transforms(fixed, moving, f"{out}InverseComposite.h5")
    assert np.abs(mine.numpy() - theirs.numpy()).max() < 1e-3


@needs_gpu
def test_a_start_from_a_file_or_parameters_is_used_as_given(tmp_path):
    first = g.register_syn(
        phantom(),
        phantom(SHIFT_MM),
        tmp_path / "a_",
        iso_mm=SPACING,
        recipe={"stages": [{"transform": "rigid", **LINEAR}]},
        outputs="split",
    )
    mat = tmp_path / "a_0GenericAffine.mat"
    syn_only = [{"transform": "syn", **SYN}]
    for initial in (mat, {"path": str(mat)}):
        info = g.register_syn(
            phantom(),
            phantom(SHIFT_MM),
            tmp_path / "b_",
            iso_mm=SPACING,
            recipe={"initial": initial, "stages": syn_only},
        )
        assert info["initial_parameters"] == first["affine_parameters"]
        assert info["recipe"]["initial"] == {"path": str(mat), "invert": False}
    info = g.register_syn(
        phantom(),
        phantom(SHIFT_MM),
        tmp_path / "c_",
        iso_mm=SPACING,
        recipe={"initial": {"path": mat, "invert": True}, "stages": syn_only},
    )
    m = np.asarray(first["affine_parameters"][:9]).reshape(3, 3)
    t = np.asarray(first["affine_parameters"][9:])
    assert np.allclose(np.reshape(info["initial_parameters"][:9], (3, 3)), np.linalg.inv(m))
    assert np.allclose(info["initial_parameters"][9:], -np.linalg.inv(m) @ t)
    params = [first["affine_parameters"], first["affine_center"]]
    info = g.register_syn(
        phantom(),
        phantom(SHIFT_MM),
        tmp_path / "d_",
        iso_mm=SPACING,
        recipe={"initial": params, "stages": [{"transform": "affine", **LINEAR}]},
    )
    assert info["initial_parameters"] == first["affine_parameters"]
    assert info["recipe"]["initial"]["center"] == list(first["affine_center"])


@needs_gpu
def test_the_capacity_check_follows_the_chain(tmp_path, monkeypatch):
    monkeypatch.setenv("FASTWARP_GPU_MEMORY_BUDGET_MB", "16")
    linear = {"stages": [{"transform": "rigid", **LINEAR}, {"transform": "affine", **LINEAR}]}
    with pytest.raises(g.GpuCapacityError):
        g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "x_", iso_mm=SPACING)
    assert not g.fits_gpu(phantom(), phantom(SHIFT_MM), iso_mm=SPACING)
    assert g.fits_gpu(phantom(), phantom(SHIFT_MM), iso_mm=SPACING, recipe=linear)
    info = g.register_syn(
        phantom(), phantom(SHIFT_MM), tmp_path / "y_", iso_mm=SPACING, recipe=linear
    )
    assert not info["deformable"]
    syn_only = {"initial": "identity", "stages": [{"transform": "syn", **SYN}]}
    assert not g.fits_gpu(phantom(), iso_mm=SPACING, recipe=syn_only)


@pytest.mark.parametrize(
    ("recipe", "match"),
    [
        ({"stages": []}, "is empty"),
        ({"stages": [{"transform": "syn"}, {"transform": "rigid"}]}, "after the SyN stage"),
        ({"stages": [{"transform": "translation"}]}, "'rigid', 'affine' or 'syn'"),
        ({"stages": [{"transform": "rigid", "cc_radius": 2}]}, "unknown key 'cc_radius'"),
        ({"stages": [{"transform": "affine", "bins": 64}]}, r"\['stages'\]\[0\]\['bins'\]"),
        ({"stages": "rigid,syn"}, "must be a list"),
        ({"rigid": {}, "initial": "com"}, "cannot be combined"),
        ({"initial": "middle"}, "recipe\\['initial'\\]"),
        ({"initial": Path("no/such/start.mat")}, "start.mat"),
        ({"initial": ((1.0,) * 5, (0.0, 0.0, 0.0))}, "12 affine parameters"),
        ({"winsorize": (0.9, 0.1)}, "recipe\\['winsorize'\\]"),
        ({"winsorize": "0.005,0.995"}, "recipe\\['winsorize'\\]"),
    ],
)
def test_a_bad_stage_list_is_refused_before_any_work(tmp_path, monkeypatch, recipe, match):
    monkeypatch.setenv("FASTWARP_GPU_ADAPTER", "no such adapter")
    with pytest.raises(ValueError, match=match):
        g.register_syn(phantom(), phantom(), tmp_path / "x_", iso_mm=SPACING, recipe=recipe)
    assert not list(tmp_path.iterdir())


def test_none_keeps_the_default_start_and_stages(tmp_path, monkeypatch):
    monkeypatch.setenv("FASTWARP_GPU_ADAPTER", "no such adapter")
    for recipe in ({"initial": None, "stages": None}, {"initial": None}, {"stages": None}):
        with pytest.raises(g.GpuUnavailableError):
            g.register_syn(phantom(), phantom(), tmp_path / "x_", iso_mm=SPACING, recipe=recipe)
    singular = {"initial": ((0.0,) * 9 + (0.0,) * 3, (0.0, 0.0, 0.0))}
    with pytest.raises(ValueError, match=r"initial transform in recipe\['initial'\] is singular"):
        g.register_syn(phantom(), phantom(), tmp_path / "x_", iso_mm=SPACING, recipe=singular)


def test_a_bad_output_layout_is_refused_before_any_work(tmp_path, monkeypatch):
    monkeypatch.setenv("FASTWARP_GPU_ADAPTER", "no such adapter")
    with pytest.raises(ValueError, match="'composite', 'split' or 'both'"):
        g.register_syn(phantom(), phantom(), tmp_path / "x_", iso_mm=SPACING, outputs="tar")
    assert g.OUTPUT_LAYOUTS == ("composite", "split", "both")


def test_two_channels_need_a_syn_stage(tmp_path, monkeypatch):
    monkeypatch.setenv("FASTWARP_GPU_ADAPTER", "no such adapter")
    with pytest.raises(ValueError, match="no SyN stage"):
        g.register_syn(
            [phantom(), phantom()],
            [phantom(), phantom()],
            tmp_path / "x_",
            iso_mm=SPACING,
            recipe={"stages": [{"transform": "rigid"}]},
        )
