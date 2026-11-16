# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""The ``fastwarp`` command line as Python ships it.

``python -m fastwarp`` and the ``fastwarp`` console script run the command line built into the
extension; they must behave like the standalone program (given by ``FASTWARP_BIN``, compared when
set) and give the same results as the Python API. The geometry tests read NIfTI files with ANTsPy
and check that the command line reads them the same way (ITK, LPS).
"""

from __future__ import annotations

import json
import os
import shutil
import struct
import subprocess
import sys
from pathlib import Path

import fastwarp as g
import numpy as np
import pytest

needs_gpu = pytest.mark.skipif(not g.gpu_available(), reason="no usable GPU adapter")


def _script() -> str | None:
    return shutil.which("fastwarp", path=str(Path(sys.executable).parent))


def _command(how: str) -> list[str]:
    if how == "module":
        return [sys.executable, "-m", "fastwarp"]
    if how == "script":
        script = _script()
        if script is None:
            pytest.skip("the fastwarp console script is not installed next to this Python")
        return [script]
    binary = os.environ.get("FASTWARP_BIN")
    if not binary:
        pytest.skip("set FASTWARP_BIN to the standalone fastwarp program to compare it")
    return [binary]


def run(*args: str | os.PathLike[str], how: str = "module") -> subprocess.CompletedProcess:
    return subprocess.run(
        [*_command(how), *(os.fspath(a) for a in args)],
        capture_output=True,
        text=True,
        timeout=900,
    )


def ok(*args: str | os.PathLike[str]) -> subprocess.CompletedProcess:
    p = run(*args)
    assert p.returncode == 0, p.stderr
    return p


@pytest.mark.parametrize("how", ["module", "script", "binary"])
def test_help_and_version(how):
    p = run("--help", how=how)
    assert p.returncode == 0, p.stderr
    for sub in ("register", "linear", "apply", "compose", "recipe", "gpu-info", "fits", "bench"):
        assert sub in p.stdout
    p = run("--version", how=how)
    assert p.returncode == 0
    assert p.stdout.strip() == f"fastwarp {g._core.VERSION}"


EXIT_CASES = [
    ([], 2),
    (["frobnicate"], 2),
    (["recipe", "--channels", "3"], 2),
    (["register", "a.mha", "b.nii", "-o", "x_"], 2),
    (["apply", "-r", "missing.nii", "-i", "m.nii", "-o", "o.nii", "-t", "t.mat"], 3),
    (["fits", "missing.nii.gz"], 3),
    (["bench", "--repeat", "0"], 2),
]


@pytest.mark.parametrize(("args", "code"), EXIT_CASES)
def test_exit_codes_are_the_same_everywhere(args, code):
    module = run(*args)
    assert module.returncode == code, module.stderr
    assert module.stdout == ""
    for how in ("script", "binary"):
        if how == "binary" and not os.environ.get("FASTWARP_BIN"):
            continue
        if how == "script" and _script() is None:
            continue
        other = run(*args, how=how)
        assert other.returncode == code, (how, other.stderr)
        assert other.stderr == module.stderr, how


def test_main_returns_the_status_in_process():
    from fastwarp.__main__ import main

    assert main(["recipe", "--channels", "3"]) == 2
    assert main(["fits", "missing.nii.gz"]) == 3


@pytest.mark.parametrize("channels", [1, 2])
def test_recipe_is_cohort_recipe(channels):
    p = ok("recipe", "--channels", str(channels))
    assert json.loads(p.stdout) == g.cohort_recipe(channels)


def test_a_bad_recipe_fails_like_the_python_api(tmp_path):
    bad = {"syn": {"iteration": [1]}}
    with pytest.raises(ValueError) as e:
        g.register_syn(_tuple_image(), _tuple_image(), tmp_path / "x_", recipe=bad)
    path = tmp_path / "recipe.json"
    path.write_text(json.dumps(bad))
    img = tmp_path / "img.nii"
    _write_nifti(img, np.zeros((4, 5, 6), dtype="<f4"), np.diag([1.0, 1.0, 1.0, 1.0]))
    p = run("register", img, img, "-o", tmp_path / "x_", "--recipe", path)
    assert p.returncode == 2
    assert str(e.value) in p.stderr


def _tuple_image(shape=(8, 8, 8)):
    return (np.zeros(shape, dtype="float32"), (1.0,) * 3, (0.0,) * 3, np.eye(3))


def _write_nifti(path, data, sform, *, slope=1.0, inter=0.0, qform_code=0, sform_code=2):
    """A NIfTI-1 file written field by field (no NIfTI library), RAS sform, no qform."""
    datatypes = {np.dtype("<i2"): (4, 16), np.dtype("<f4"): (16, 32)}
    code, bitpix = datatypes[data.dtype]
    spacing = np.linalg.norm(np.asarray(sform)[:3, :3], axis=0)
    h = bytearray(352)
    struct.pack_into("<i", h, 0, 348)
    struct.pack_into("<8h", h, 40, 3, *data.shape, 1, 1, 1, 1)
    struct.pack_into("<hh", h, 70, code, bitpix)
    struct.pack_into("<8f", h, 76, 1.0, *spacing, 1.0, 1.0, 1.0, 1.0)
    struct.pack_into("<fff", h, 108, 352.0, slope, inter)
    struct.pack_into("<B", h, 123, 2)
    struct.pack_into("<hh", h, 252, qform_code, sform_code)
    for row in range(3):
        struct.pack_into("<4f", h, 280 + 16 * row, *np.asarray(sform)[row])
    h[344:348] = b"n+1\0"
    Path(path).write_bytes(bytes(h) + data.tobytes(order="F"))


def _rot(deg: float) -> np.ndarray:
    a = np.deg2rad(deg)
    return np.array([[np.cos(a), -np.sin(a), 0.0], [np.sin(a), np.cos(a), 0.0], [0.0, 0.0, 1.0]])


@pytest.fixture(scope="module")
def ants():
    return pytest.importorskip("ants")


@pytest.fixture(scope="module")
def identity(ants, tmp_path_factory):
    path = tmp_path_factory.mktemp("identity") / "identity.mat"
    ants.write_transform(
        ants.create_ants_transform(transform_type="AffineTransform", dimension=3), str(path)
    )
    return path


def _same_geometry(a, b):
    assert a.shape == b.shape
    np.testing.assert_allclose(a.spacing, b.spacing, atol=1e-5)
    np.testing.assert_allclose(a.origin, b.origin, atol=1e-4)
    np.testing.assert_allclose(a.direction, b.direction, atol=1e-6)


def _identity_round_trip(ants, identity, src, tmp_path):
    out = tmp_path / "out.nii.gz"
    ok("apply", "-r", src, "-i", src, "-o", out, "-t", identity, "-n", "nearestNeighbor")
    theirs = ants.image_read(str(src))
    mine = ants.image_read(str(out))
    _same_geometry(mine, theirs)
    np.testing.assert_array_equal(mine.numpy(), theirs.numpy().astype("float32"))


def test_an_ants_written_oblique_image_reads_as_in_ants(ants, identity, tmp_path):
    data = np.random.default_rng(3).normal(size=(14, 11, 9)).astype("float32")
    img = ants.from_numpy(data, origin=(-20.0, 15.0, -10.0), spacing=(1.1, 0.9, 1.3))
    img.set_direction(_rot(17) @ np.diag([1.0, -1.0, 1.0]))
    src = tmp_path / "oblique.nii.gz"
    ants.image_write(img, str(src))
    _identity_round_trip(ants, identity, src, tmp_path)


def test_a_scaled_sform_only_image_reads_as_in_ants(ants, identity, tmp_path):
    rot = _rot(-23) @ np.array([[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]])
    sform = np.eye(4)
    sform[:3, :3] = rot @ np.diag([-1.2, 0.8, 1.5])
    sform[:3, 3] = (30.0, -12.5, 7.25)
    data = np.arange(7 * 6 * 5, dtype="<i2").reshape(7, 6, 5) - 50
    src = tmp_path / "sform.nii"
    _write_nifti(src, data, sform, slope=2.0, inter=-3.0)
    _identity_round_trip(ants, identity, src, tmp_path)
    np.testing.assert_array_equal(ants.image_read(str(src)).numpy(), data * 2.0 - 3.0)


@pytest.fixture(scope="module")
def scene(ants, tmp_path_factory):
    """An oblique moving image, a reference on another grid, and an ANTs affine between them."""
    d = tmp_path_factory.mktemp("cli_post")
    rng = np.random.default_rng(0)
    moving = ants.from_numpy(
        rng.normal(size=(30, 34, 26)).astype("float32") + 50.0,
        origin=(-20.0, 15.0, -10.0),
        spacing=(1.1, 0.9, 1.3),
        direction=_rot(12),
    )
    reference = ants.from_numpy(
        np.zeros((36, 30, 22), dtype="float32"),
        origin=(-22.0, 12.0, -14.0),
        spacing=(0.8, 1.0, 1.5),
        direction=_rot(-7),
    )
    tx = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=np.array([[1.05, 0.04, 0.0], [-0.03, 0.97, 0.02], [0.01, 0.0, 1.02]]),
        translation=(1.3, -2.1, 0.7),
        center=(2.0, 30.0, 10.0),
    )
    shift = ants.create_ants_transform(
        transform_type="AffineTransform", dimension=3, translation=(3.0, 0.0, -1.0)
    )
    paths = {"moving": d / "moving.nii.gz", "reference": d / "reference.nii.gz"}
    ants.image_write(moving, str(paths["moving"]))
    ants.image_write(reference, str(paths["reference"]))
    paths["affine"] = d / "affine.mat"
    paths["shift"] = d / "shift.mat"
    ants.write_transform(tx, str(paths["affine"]))
    ants.write_transform(shift, str(paths["shift"]))
    return d, paths


@pytest.mark.parametrize("interp", g.INTERPOLATORS)
def test_apply_equals_the_python_api_and_ants(ants, scene, interp):
    d, p = scene
    order = [p["affine"], p["shift"]]
    out = d / f"cli_{interp}.nii.gz"
    tx = [a for t in order for a in ("-t", t)]
    ok("apply", "-r", p["reference"], "-i", p["moving"], "-o", out, *tx, "-n", interp)
    moving = ants.image_read(str(p["moving"]))
    reference = ants.image_read(str(p["reference"]))
    api = g.apply_transforms(moving, reference, [str(t) for t in order], interpolation=interp)
    theirs = ants.apply_transforms(
        fixed=reference, moving=moving, transformlist=[str(t) for t in order], interpolator=interp
    )
    mine = ants.image_read(str(out))
    _same_geometry(mine, reference)
    np.testing.assert_array_equal(mine.numpy(), api.numpy())
    assert np.abs(mine.numpy() - theirs.numpy()).max() < 1e-3


def test_compose_equals_the_python_api(ants, scene):
    d, p = scene
    order = [str(p["shift"]), str(p["affine"])]
    out = d / "cli_field.nii.gz"
    ok("compose", "-r", p["reference"], "-o", out, "-t", order[0], "-t", order[1])
    reference = ants.image_read(str(p["reference"]))
    api = g.compose_field(order, reference, d / "api_field.nii.gz")
    np.testing.assert_array_equal(
        ants.image_read(str(out)).numpy(), ants.image_read(str(api)).numpy()
    )


SHAPE = (28, 32, 24)
SPACING = 4.0
SHIFT_MM = np.array([4.0, -3.0, 2.0])
SMALL_RECIPE = {
    "rigid": {"iterations": [60, 30], "shrink_factors": [2, 1], "smoothing_sigmas": [1, 0]},
    "affine": {"iterations": [60, 30], "shrink_factors": [2, 1], "smoothing_sigmas": [1, 0]},
    "syn": {"iterations": [10, 5], "shrink_factors": [2, 1], "smoothing_sigmas": [1, 0]},
}


def _head(shift_mm=(0.0, 0.0, 0.0)) -> np.ndarray:
    pts = np.indices(SHAPE, dtype=np.float64) * SPACING
    s = np.asarray(shift_mm)

    def ellipsoid(c, r):
        return sum(((pts[a] - c[a] - s[a]) / r[a]) ** 2 for a in range(3)) <= 1.0

    vol = np.zeros(SHAPE)
    vol[ellipsoid((56, 64, 48), (44, 52, 38))] = 0.3
    vol[ellipsoid((56, 64, 48), (38, 46, 32))] = 0.7
    vol[ellipsoid((48, 70, 50), (8, 12, 7))] = 0.2
    vol[ellipsoid((56, 40, 40), (10, 8, 8))] = 1.0
    vol += 0.01 * np.random.default_rng(0).standard_normal(SHAPE)
    return vol.astype("float32")


@pytest.fixture(scope="module")
def registered(ants, tmp_path_factory):
    if not g.gpu_available():
        pytest.skip("no usable GPU adapter")
    d = tmp_path_factory.mktemp("cli_register")
    files = {}
    for name, shift in (("fixed", (0.0, 0.0, 0.0)), ("moving", SHIFT_MM)):
        img = ants.from_numpy(_head(shift), spacing=(SPACING,) * 3)
        files[name] = d / f"{name}.nii.gz"
        ants.image_write(img, str(files[name]))
    recipe = d / "recipe.json"
    recipe.write_text(json.dumps(SMALL_RECIPE))
    prefix = d / "out" / "sub_"
    p = ok(
        "register",
        files["fixed"],
        files["moving"],
        "-o",
        f"{prefix}",
        "--iso-mm",
        str(SPACING),
        "--recipe",
        recipe,
        "--json",
    )
    return d, files, prefix, json.loads(p.stdout)


@needs_gpu
def test_register_matches_the_python_api(ants, registered):
    d, files, _, info = registered
    fixed = ants.image_read(str(files["fixed"]))
    moving = ants.image_read(str(files["moving"]))
    api = g.register_syn(fixed, moving, d / "api" / "sub_", iso_mm=SPACING, recipe=SMALL_RECIPE)
    assert info["recipe"] == api["recipe"]
    assert tuple(info["grid"]) == tuple(api["grid"])
    np.testing.assert_allclose(info["affine_parameters"], api["affine_parameters"], atol=1e-4)
    np.testing.assert_allclose(np.asarray(info["affine_parameters"])[9:], SHIFT_MM, atol=0.5)


@needs_gpu
def test_the_cli_transform_is_read_by_the_python_api_and_ants(ants, registered):
    d, files, prefix, _ = registered
    composite = f"{prefix}Composite.h5"
    fixed = ants.image_read(str(files["fixed"]))
    moving = ants.image_read(str(files["moving"]))
    out = d / "warped.nii.gz"
    ok("apply", "-r", files["fixed"], "-i", files["moving"], "-o", out, "-t", composite)
    api = g.apply_transforms(moving, fixed, composite)
    theirs = ants.apply_transforms(fixed=fixed, moving=moving, transformlist=[composite])
    mine = ants.image_read(str(out)).numpy()
    np.testing.assert_array_equal(mine, api.numpy())
    assert np.abs(mine - theirs.numpy()).max() < 1e-3
    before = np.corrcoef(fixed.numpy().ravel(), moving.numpy().ravel())[0, 1]
    after = np.corrcoef(fixed.numpy().ravel(), mine.ravel())[0, 1]
    assert after > before


@needs_gpu
def test_fits_agrees_with_fits_gpu(ants, registered):
    _, files, _, _ = registered
    fixed = ants.image_read(str(files["fixed"]))
    moving = ants.image_read(str(files["moving"]))
    p = run("fits", files["fixed"], files["moving"], "--iso-mm", str(SPACING), "--json")
    assert p.returncode == (0 if g.fits_gpu(fixed, moving, iso_mm=SPACING) else 5)
    assert json.loads(p.stdout)["grid"] == list(g.iso_grid_dims(fixed, SPACING))


STAGE_LIST = {
    "initial": "identity",
    "stages": [{"transform": "syn", **SMALL_RECIPE["syn"]}],
    "winsorize": None,
}


@needs_gpu
def test_a_stage_list_recipe_and_split_outputs_match_the_python_api(ants, registered):
    d, files, _, _ = registered
    recipe = d / "stages.json"
    recipe.write_text(json.dumps(STAGE_LIST))
    prefix = d / "split" / "sub_"
    p = ok(
        "register",
        files["fixed"],
        files["moving"],
        "-o",
        f"{prefix}",
        "--iso-mm",
        str(SPACING),
        "--recipe",
        recipe,
        "--outputs",
        "split",
        "--json",
    )
    info = json.loads(p.stdout)
    names = [Path(f).name for f in info["outputs"]]
    assert names == ["sub_0GenericAffine.mat", "sub_1Warp.nii.gz", "sub_1InverseWarp.nii.gz"]
    assert info["deformable"] and info["linear_stages"] == []
    fixed = ants.image_read(str(files["fixed"]))
    moving = ants.image_read(str(files["moving"]))
    api = g.register_syn(
        fixed, moving, d / "split_api" / "sub_", iso_mm=SPACING, recipe=STAGE_LIST, outputs="split"
    )
    assert info["recipe"] == api["recipe"]
    assert info["affine_parameters"] == api["affine_parameters"]
    for f in names[1:]:
        mine = ants.image_read(str(d / "split" / f)).numpy()
        theirs = ants.image_read(str(d / "split_api" / f)).numpy()
        np.testing.assert_array_equal(mine, theirs)
    out = d / "split_warped.nii.gz"
    tx = [f"{prefix}1Warp.nii.gz", f"{prefix}0GenericAffine.mat"]
    ok("apply", "-r", files["fixed"], "-i", files["moving"], "-o", out, "-t", tx[0], "-t", tx[1])
    theirs = ants.apply_transforms(fixed=fixed, moving=moving, transformlist=tx)
    assert np.abs(ants.image_read(str(out)).numpy() - theirs.numpy()).max() < 1e-3


@needs_gpu
def test_fits_checks_the_stages_of_the_recipe(ants, registered, monkeypatch):
    d, files, _, _ = registered
    monkeypatch.setenv("FASTWARP_GPU_MEMORY_BUDGET_MB", "1")
    linear = d / "linear.json"
    linear.write_text(json.dumps({"stages": [{"transform": "rigid"}]}))
    common = ("fits", files["fixed"], files["moving"], "--iso-mm", str(SPACING))
    assert run(*common).returncode == 5
    assert run(*common, "--recipe", linear).returncode == 0


def test_apply_inverts_an_affine_like_ants(ants, tmp_path):
    img = ants.from_numpy(_head(), spacing=(SPACING,) * 3)
    src = tmp_path / "img.nii.gz"
    ants.image_write(img, str(src))
    tx = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=np.array([[1.04, 0.02, 0.0], [-0.01, 0.97, 0.0], [0.0, 0.03, 1.0]]),
        translation=(2.0, -1.0, 3.0),
        center=(50.0, 60.0, 40.0),
    )
    mat = tmp_path / "a.mat"
    ants.write_transform(tx, str(mat))
    out = tmp_path / "inv.nii.gz"
    ok("apply", "-r", src, "-i", src, "-o", out, "-t", f"[{mat},1]")
    theirs = ants.apply_transforms(
        fixed=img, moving=img, transformlist=[str(mat)], whichtoinvert=[True]
    )
    assert np.abs(ants.image_read(str(out)).numpy() - theirs.numpy()).max() < 1e-3


@needs_gpu
def test_the_default_grid_is_the_finest_fixed_spacing(tmp_path):
    sform = np.diag([1.2, 0.8, 1.5, 1.0])
    src = tmp_path / "aniso.nii"
    _write_nifti(src, np.zeros((20, 30, 16), dtype="<f4"), sform)
    info = json.loads(ok("fits", src, "--json").stdout)
    assert info["iso_mm"] == pytest.approx(0.8)
    image = (np.zeros((20, 30, 16)), (1.2, 0.8, 1.5), (0.0, 0.0, 0.0), np.eye(3))
    assert info["grid"] == list(g.iso_grid_dims(image, 0.8))


@needs_gpu
def test_gpu_info_is_the_python_gpu_info(monkeypatch):
    monkeypatch.setenv("FASTWARP_GPU_MEMORY_BUDGET_MB", "4096")
    assert json.loads(ok("gpu-info").stdout) == g.gpu_info()


def test_no_gpu_gives_exit_4(monkeypatch):
    monkeypatch.setenv("FASTWARP_GPU_BACKEND", "nonsense")
    p = run("gpu-info")
    assert p.returncode == 4
    assert p.stderr.startswith("fastwarp: error: ")
