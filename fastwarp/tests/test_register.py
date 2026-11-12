# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""End-to-end tests on a synthetic phantom (no patient data).

GPU tests skip without a usable adapter; CI runs them on Mesa's lavapipe with
FASTWARP_GPU_ALLOW_SOFTWARE=1.
"""

from __future__ import annotations

import threading
from pathlib import Path

import fastwarp as g
import numpy as np
import pytest

SPACING = 2.0
SHAPE = (56, 64, 48)
SHIFT_MM = np.array([4.0, -3.0, 2.0])

needs_gpu = pytest.mark.skipif(not g.gpu_available(), reason="no usable GPU adapter")


def phantom(shift_mm=(0.0, 0.0, 0.0)) -> tuple:
    """A sharp-edged head: scalp, brain, two unequal ventricles, a bright nucleus; LPS identity.

    (Smooth Gaussian blobs leave the affine scale poorly determined under MI; edges fix it.)
    """
    pts = np.indices(SHAPE, dtype=np.float64) * SPACING
    s = np.asarray(shift_mm)

    def ellipsoid(c, r):
        return sum(((pts[a] - c[a] - s[a]) / r[a]) ** 2 for a in range(3)) <= 1.0

    vol = np.zeros(SHAPE)
    vol[ellipsoid((56, 64, 48), (44, 52, 38))] = 0.3
    vol[ellipsoid((56, 64, 48), (38, 46, 32))] = 0.7
    vol[ellipsoid((48, 70, 50), (6, 10, 5))] = 0.2
    vol[ellipsoid((66, 70, 50), (5, 9, 5))] = 0.2
    vol[ellipsoid((56, 40, 40), (8, 6, 6))] = 1.0
    vol += 0.01 * np.random.default_rng(0).standard_normal(SHAPE)
    return (vol, (SPACING,) * 3, (0.0, 0.0, 0.0), np.eye(3))


@pytest.fixture(scope="module")
def result(tmp_path_factory):
    out = tmp_path_factory.mktemp("reg") / "sub_"
    info = g.register_syn(phantom(), phantom(SHIFT_MM), out, iso_mm=SPACING, write_warped=True)
    return out, info


@needs_gpu
def test_outputs_written(result):
    out, info = result
    for suffix in ("Composite.h5", "InverseComposite.h5", "0GenericAffine.mat", "_Warped.nii.gz"):
        f = Path(f"{out}{suffix}")
        assert f.is_file() and f.stat().st_size > 0, suffix
    assert tuple(info["grid"]) == g.iso_grid_dims(phantom(), SPACING)
    assert info["peak_gpu_bytes"] > 0
    assert [lvl[0] for lvl in info["levels"]] == [0, 1, 2, 3]


@needs_gpu
def test_affine_recovers_the_shift(result):
    _, info = result
    # ITK affine maps fixed points to moving points: translation ~ +shift for a pure shift.
    p = np.asarray(info["affine_parameters"])
    np.testing.assert_allclose(p[:9].reshape(3, 3), np.eye(3), atol=0.01)
    np.testing.assert_allclose(p[9:], SHIFT_MM, atol=0.3)


@needs_gpu
def test_two_channels(tmp_path):
    fixed = [phantom(), phantom()]
    moving = [phantom(SHIFT_MM), phantom(SHIFT_MM)]
    info = g.register_syn(fixed, moving, tmp_path / "mc_", iso_mm=SPACING)
    assert Path(f"{tmp_path}/mc_Composite.h5").is_file()
    np.testing.assert_allclose(np.asarray(info["affine_parameters"])[9:], SHIFT_MM, atol=0.3)


@needs_gpu
def test_resamples_to_iso_grid(tmp_path):
    info = g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "iso_", iso_mm=3.0)
    assert tuple(info["grid"]) == g.iso_grid_dims(phantom(), 3.0)
    assert info["resample_s"] >= 0.0


@needs_gpu
def test_cancel_before_start(tmp_path):
    tok = g.CancelToken()
    tok.cancel()
    assert tok.cancelled
    with pytest.raises(g.RegistrationCancelled):
        g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "c_", iso_mm=SPACING, cancel=tok)


@needs_gpu
def test_cancel_from_another_thread(tmp_path):
    # 1 mm grid (8x the voxels): long enough on any GPU for the timer to land mid-run.
    tok = g.CancelToken()
    timer = threading.Timer(0.2, tok.cancel)
    timer.start()
    try:
        with pytest.raises(g.RegistrationCancelled):
            g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "t_", iso_mm=1.0, cancel=tok)
    finally:
        timer.cancel()


@needs_gpu
def test_capacity_refused_before_work(tmp_path, monkeypatch):
    monkeypatch.setenv("FASTWARP_GPU_MEMORY_BUDGET_MB", "1")
    assert not g.fits_gpu(phantom(), iso_mm=SPACING)
    with pytest.raises(g.GpuCapacityError):
        g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "cap_", iso_mm=SPACING)
    assert not Path(f"{tmp_path}/cap_Composite.h5").exists()


@needs_gpu
def test_gpu_info():
    info = g.gpu_info()
    assert info["name"] and info["backend"] and info["memory_budget_bytes"] > 0


def test_bad_inputs(tmp_path):
    with pytest.raises(ValueError):
        g.register_syn(
            (np.zeros((4, 4)), (1.0,) * 3, (0.0,) * 3, np.eye(3)), phantom(), tmp_path / "x"
        )
    with pytest.raises(ValueError):
        g.register_syn([phantom(), phantom()], phantom(), tmp_path / "x")
    with pytest.raises(ValueError):
        g.register_syn(phantom(), phantom(), tmp_path / "x", iso_mm=0.0)


def test_iso_grid_dims_rule():
    img = (np.zeros((256, 256, 176)), (1.0, 1.0, 1.0), (0.0,) * 3, np.eye(3))
    assert g.iso_grid_dims(img, 1.0) == (256, 256, 176)
    img = (np.zeros((394, 466, 378)), (0.5, 0.5, 0.5), (0.0,) * 3, np.eye(3))
    assert g.iso_grid_dims(img, 1.0) == (197, 233, 189)  # the MNI 2009b 0.5 mm template


@needs_gpu
def test_ants_reads_the_transforms(result):
    ants = pytest.importorskip("ants")
    out, _ = result
    f = ants.from_numpy(phantom()[0], spacing=(SPACING,) * 3)
    m = ants.from_numpy(phantom(SHIFT_MM)[0], spacing=(SPACING,) * 3)
    w = ants.apply_transforms(fixed=f, moving=m, transformlist=[f"{out}Composite.h5"])
    before = np.corrcoef(f.numpy().ravel(), m.numpy().ravel())[0, 1]
    after = np.corrcoef(f.numpy().ravel(), w.numpy().ravel())[0, 1]
    assert after > 0.99 > before
    back = ants.apply_transforms(fixed=m, moving=w, transformlist=[f"{out}InverseComposite.h5"])
    assert np.corrcoef(m.numpy().ravel(), back.numpy().ravel())[0, 1] > 0.98


def _hashes(prefix: Path) -> tuple[str, str]:
    import hashlib

    return tuple(  # type: ignore[return-value]
        hashlib.sha256(Path(f"{prefix}{s}").read_bytes()).hexdigest()
        for s in ("Composite.h5", "InverseComposite.h5")
    )


@needs_gpu
def test_runs_are_deterministic(tmp_path):
    g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "a_", iso_mm=SPACING)
    g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "b_", iso_mm=SPACING)
    assert _hashes(tmp_path / "a_") == _hashes(tmp_path / "b_")


@needs_gpu
def test_float32_input_equals_the_same_values_as_float64(tmp_path):
    def as_dtype(img: tuple, dtype) -> tuple:
        return (img[0].astype(np.float32).astype(dtype), *img[1:])

    f, m = phantom(), phantom(SHIFT_MM)
    g.register_syn(as_dtype(f, np.float32), as_dtype(m, np.float32), tmp_path / "f32_", iso_mm=2.0)
    g.register_syn(as_dtype(f, np.float64), as_dtype(m, np.float64), tmp_path / "f64_", iso_mm=2.0)
    assert _hashes(tmp_path / "f32_") == _hashes(tmp_path / "f64_")


def test_float32_fortran_data_is_passed_without_a_copy():
    arr = np.asfortranarray(np.zeros((4, 5, 6), dtype=np.float32))
    flat = g._image_args((arr, (1.0,) * 3, (0.0,) * 3, np.eye(3)))[0]
    assert flat.dtype == np.float32 and np.shares_memory(flat, arr)


@needs_gpu
def test_non_finite_voxels_are_refused(tmp_path):
    bad = phantom()
    bad[0][3, 3, 3] = np.nan
    with pytest.raises(g.RegistrationError, match="non-finite"):
        g.register_syn(bad, phantom(SHIFT_MM), tmp_path / "n_", iso_mm=SPACING)


def test_a_missing_output_folder_is_refused_before_any_work(tmp_path):
    from fastwarp import _core

    args = g._image_args(phantom())
    with pytest.raises(ValueError, match="does not exist"):
        _core.register_syn([args], [args], str(tmp_path / "missing" / "x_"))


@needs_gpu
def test_the_wrapper_creates_the_output_folder(tmp_path):
    g.register_syn(phantom(), phantom(SHIFT_MM), tmp_path / "new" / "dir" / "x_", iso_mm=3.0)
    assert (tmp_path / "new" / "dir" / "x_Composite.h5").is_file()


def test_integer_images_are_accepted():
    arr = np.ones((4, 5, 6), dtype=np.int16)
    assert g._image_args((arr, (1.0,) * 3, (0.0,) * 3, np.eye(3)))[0].dtype == np.float32


@needs_gpu
def test_the_gpu_survives_many_opens_in_one_process():
    """Every call opens and closes a device. NVIDIA's Linux Vulkan driver used to vanish after the
    tenth (the loader could not load it again); a process-wide idle instance keeps it loaded."""
    names = {g.gpu_info()["name"] for _ in range(15)}
    assert len(names) == 1
