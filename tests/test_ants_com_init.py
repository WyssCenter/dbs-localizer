# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
``--initial-moving-transform`` argument order (`specs/03` §5, `specs/04` §5).

ANTs takes ``[fixedImage,movingImage,initializationFeature]`` — **fixed first** — which is
the opposite of the ``moving, fixed`` argument order used by the Wyss DBS Localizer argv builders. Passing
them in builder order makes the centre-of-mass initialiser translate the wrong way, doubling a
gross offset instead of removing it.
"""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

import nibabel as nib
import numpy as np
import pytest

from dbsloc.bundled_bin import which_ants_binary


def _hr():
    return pytest.importorskip("dbsloc_reconstruct", reason="PyO3 extension not built")


def build_ants_registration_argv(mov, fix, pfx):
    return list(_hr().ants_coreg_argv(str(Path(mov)), str(Path(fix)), str(pfx)))


def build_ants_syn_registration_argv(mov, fix, pfx):
    return list(_hr().ants_syn_argv(str(Path(mov)), str(Path(fix)), str(pfx)))


_MOV_IN = "/tmp/mov.nii.gz"
_FIX_IN = "/tmp/fix.nii.gz"
_PFX = "/tmp/out"

_MOV = str(Path(_MOV_IN))
_FIX = str(Path(_FIX_IN))


def _init_arg(argv: list[str]) -> str:
    i = argv.index("--initial-moving-transform")
    return argv[i + 1]


def test_coreg_initial_moving_transform_is_fixed_then_moving() -> None:
    argv = build_ants_registration_argv(_MOV_IN, _FIX_IN, _PFX)
    assert _init_arg(argv) == f"[{_FIX},{_MOV},1]"


def test_norm_initial_moving_transform_is_fixed_then_moving() -> None:
    argv = build_ants_syn_registration_argv(_MOV_IN, _FIX_IN, _PFX)
    assert _init_arg(argv) == f"[{_FIX},{_MOV},1]"


def test_init_order_is_opposite_of_metric_order() -> None:
    """The metric takes ``[fixed,moving,…]``; the initialiser must use the same leading image."""
    argv = build_ants_registration_argv(_MOV_IN, _FIX_IN, _PFX)
    metric = next(t for t in argv if t.startswith("MI["))
    assert metric.startswith(f"MI[{_FIX},{_MOV},")
    assert _init_arg(argv).startswith(f"[{_FIX},{_MOV},")


def _blob(
    shape: tuple[int, int, int], centre_mm: tuple[float, float, float], aff: np.ndarray
) -> np.ndarray:
    ii, jj, kk = np.meshgrid(*[np.arange(s) for s in shape], indexing="ij")
    w = np.stack([ii, jj, kk, np.ones_like(ii)], -1).astype(np.float64) @ aff.T
    d = np.linalg.norm(w[..., :3] - np.array(centre_mm), axis=-1)
    return np.asarray(
        np.clip(1.0 - (d - 8.0) / 2.0, 0.0, 1.0) * 100.0, dtype=np.float32
    )


@pytest.mark.slow
@pytest.mark.skipif(
    which_ants_binary("antsRegistration") is None,
    reason="antsRegistration not found (bundled or PATH)",
)
def test_com_init_removes_gross_offset() -> None:
    """
    Zero-iteration rigid registration, so the warped output reflects **only** the
    initialiser. A 24 mm pure translation must collapse to ~0 mm; the swapped order
    yields ~48 mm.
    """
    ants = which_ants_binary("antsRegistration")
    assert ants is not None
    aff = np.diag([2.0, 2.0, 2.0, 1.0])
    aff[:3, 3] = [-64.0, -64.0, -64.0]
    shape = (64, 64, 64)
    fixed_c = (0.0, 0.0, 0.0)
    moving_c = (24.0, 0.0, 0.0)

    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        fix_p = tdp / "fix.nii.gz"
        mov_p = tdp / "mov.nii.gz"
        nib.save(nib.Nifti1Image(_blob(shape, fixed_c, aff), aff), fix_p)
        nib.save(nib.Nifti1Image(_blob(shape, moving_c, aff), aff), mov_p)
        pfx = tdp / "out"
        argv = [
            ants,
            "--dimensionality",
            "3",
            "--output",
            f"[{pfx},{pfx}_Warped.nii.gz]",
            "--initial-moving-transform",
            f"[{fix_p},{mov_p},1]",
            "--transform",
            "Rigid[0.1]",
            "--metric",
            f"MI[{fix_p},{mov_p},1,32,Regular,0.25]",
            "--convergence",
            "[0,1e-6,10]",
            "--shrink-factors",
            "1",
            "--smoothing-sigmas",
            "0vox",
        ]
        r = subprocess.run(argv, capture_output=True, text=True, check=False)
        if r.returncode != 0:
            pytest.fail(
                f"antsRegistration rc={r.returncode}: {(r.stderr or r.stdout)[-1500:]}"
            )
        warped = tdp / "out_Warped.nii.gz"
        assert warped.is_file()
        w = np.asanyarray(nib.load(warped).dataobj, dtype=np.float64)
        idx = np.array(np.where(w > 50.0))
        assert idx.size > 0, "warped volume is empty"
        ras = (aff @ np.vstack([idx, np.ones(idx.shape[1])]))[:3].T
        err = float(np.linalg.norm(ras.mean(0) - np.array(fixed_c)))
        assert err < 1.0, (
            f"COM init left {err:.2f} mm of the 24 mm offset (order swapped?)"
        )


def _brain(
    shape: tuple[int, int, int], aff: np.ndarray, centre_mm: tuple[float, float, float]
) -> np.ndarray:
    ii, jj, kk = np.meshgrid(*[np.arange(s) for s in shape], indexing="ij")
    w = np.stack([ii, jj, kk, np.ones_like(ii)], -1).astype(np.float64) @ aff.T
    d = np.sqrt(
        (
            ((w[..., :3] - np.array(centre_mm)) ** 2) / np.array([1.0, 1.15, 0.9]) ** 2
        ).sum(-1)
    )
    v = np.clip(1.0 - (d - 26.0) / 5.0, 0.0, 1.0) * (
        0.6 + 0.4 * np.sin(w[..., 0] / 7.0) * np.cos(w[..., 1] / 9.0)
    )
    return np.asarray(v * 900.0 + 20.0, dtype=np.float32)


@pytest.mark.slow
@pytest.mark.skipif(
    which_ants_binary("antsRegistration") is None,
    reason="antsRegistration not found (bundled or PATH)",
)
def test_far_apart_headers_need_no_prealignment() -> None:
    """
    A post-op CT whose header origin puts the anatomy 60 mm from the anchor must coregister
    from the production argv alone. This replaces the ``_com_aligned_moving_nifti``
    pre-alignment copy that existed only to compensate for the swapped initialiser order.
    """
    ants = which_ants_binary("antsRegistration")
    assert ants is not None
    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        aff_f = np.diag([1.5, 1.5, 1.5, 1.0])
        aff_f[:3, 3] = [-60.0, -60.0, -60.0]
        fix_p = tdp / "fix.nii.gz"
        nib.save(
            nib.Nifti1Image(_brain((80, 80, 80), aff_f, (0.0, 0.0, 0.0)), aff_f), fix_p
        )

        aff_m = np.diag([1.0, 1.0, 1.0, 1.0])
        aff_m[:3, 3] = [0.0, -60.0, -60.0]
        mov_p = tdp / "mov.nii.gz"
        nib.save(
            nib.Nifti1Image(_brain((120, 120, 120), aff_m, (60.0, 0.0, 0.0)), aff_m),
            mov_p,
        )

        pfx = tdp / "out"
        argv = build_ants_registration_argv(mov_p, fix_p, pfx)
        argv[0] = ants
        r = subprocess.run(argv, capture_output=True, text=True, check=False)
        if r.returncode != 0:
            pytest.fail(
                f"antsRegistration rc={r.returncode}: {(r.stderr or r.stdout)[-1500:]}"
            )
        w = np.asanyarray(nib.load(tdp / "out_Warped.nii.gz").dataobj, dtype=np.float64)
        idx = np.array(np.where(w > 300.0))
        assert idx.size > 0, "warped volume is empty"
        ras = (aff_f @ np.vstack([idx, np.ones(idx.shape[1])]))[:3].T
        err = float(np.linalg.norm(ras.mean(0)))
        assert err < 3.0, f"60 mm header offset left {err:.2f} mm of misalignment"
