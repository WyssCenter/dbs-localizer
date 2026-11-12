# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
Golden-style checks vs real ANTs (when ``antsRegistration`` on PATH).

Synthetic tiny BLOB pair — no LeadTutor bulk in git. Ties to ``build_ants_registration_argv``
kinds: rigid + MI, same *structure* as
``dbsloc_reconstruct.ants_coreg_argv`` (Rust ``build_ants_ct_coreg_argv``), but *shorter* convergence
for test speed. Full argv mirrors production in ``test_linear_argv_matches_production_kinds`` (no run).

**Parity** = correlation / NCC in overlap vs fixed, not bit-identical voxels
(ANTs/ITK version, thread count can shift floats slightly).
"""

from __future__ import annotations

import os
from pathlib import Path

import nibabel as nib
import numpy as np
import pytest


def _hr():
    return pytest.importorskip("dbsloc_reconstruct", reason="PyO3 extension not built")


ANTS_RANDOM_SEED = 20260910


def run_argv(argv, *, check, capture, env):
    import subprocess

    return subprocess.run(argv, check=check, capture_output=capture, text=True, env=env)


def build_ants_registration_argv(mov, fix, pfx):
    return list(_hr().ants_coreg_argv(str(Path(mov)), str(Path(fix)), str(pfx)))


_MAX_RMSE = 50.0
_MIN_MASKED_PEARSON = 0.80
_MIN_MASKED_NCC = 0.45


def _ants_registration_bin() -> str | None:
    from dbsloc.bundled_bin import which_ants_binary

    return which_ants_binary("antsRegistration")


def _has_ants_registration() -> bool:
    return _ants_registration_bin() is not None


def _eye_mm(scale: tuple[float, float, float] = (2.0, 2.0, 2.0)) -> np.ndarray:
    a = np.eye(4, dtype=np.float64)
    a[0, 0], a[1, 1], a[2, 2] = scale
    return a


def _sphere_volume(
    shape: tuple[int, int, int],
    center_zyx: tuple[float, float, float],
    r: float,
    fore: float,
    back: float,
) -> np.ndarray:
    zz, yy, xx = np.ogrid[0 : shape[0], 0 : shape[1], 0 : shape[2]]
    d2 = (
        (zz - center_zyx[0]) ** 2
        + (yy - center_zyx[1]) ** 2
        + (xx - center_zyx[2]) ** 2
    )
    out = np.full(shape, back, dtype=np.float32)
    out[d2 <= r**2] = fore
    return out


def _sphere_shell_mask(
    shape: tuple[int, int, int],
    center_zyx: tuple[float, float, float],
    r0: float,
    r1: float,
) -> np.ndarray:
    """Voxels in spherical shell; intensities in shell vary (edge of blob) → correlation well-defined."""
    zz, yy, xx = np.ogrid[0 : shape[0], 0 : shape[1], 0 : shape[2]]
    d2 = (
        (zz - center_zyx[0]) ** 2
        + (yy - center_zyx[1]) ** 2
        + (xx - center_zyx[2]) ** 2
    )
    d = np.sqrt(np.maximum(d2, 0.0))
    return (d >= r0) & (d <= r1)


def _masked_pearson(a: np.ndarray, b: np.ndarray, mask: np.ndarray) -> float:
    am = a[mask].ravel()
    bm = b[mask].ravel()
    n = int(am.size)
    if n < 32:
        return 0.0
    if float(am.std()) < 1e-5 or float(bm.std()) < 1e-5:
        return 0.0
    c = np.corrcoef(am, bm)
    v = float(c[0, 1])
    return 0.0 if np.isnan(v) else v


def _masked_ncc(a: np.ndarray, b: np.ndarray, mask: np.ndarray) -> float:
    """Local ``CC``-style: zero-mean / unit-norm in mask, then dot / n."""
    am = a[mask].ravel().astype(np.float64)
    bm = b[mask].ravel().astype(np.float64)
    am = am - am.mean()
    bm = bm - bm.mean()
    da = np.sqrt((am * am).sum())
    db = np.sqrt((bm * bm).sum())
    if da < 1e-9 or db < 1e-9:
        return 0.0
    return float((am * bm).sum() / (da * db))


def _build_fast_ct_anchor_like_argv(
    moving: Path, fixed: Path, out_prefix: Path
) -> list[str]:
    """
    Same registration *family* as production CT→anchor (rigid, MI) but
    few iterations so CI with ANTs finishes in ~tens of seconds.

    Seeded like production. ANTs seeds from the system clock by default and MI samples voxels
    randomly, so the metric assertions below drift between runs — measured once at shell RMSE
    63.1 against a 50.0 threshold that normally passes with room to spare. The seed narrows
    that drift but does not remove it, since this runs with 2 ITK threads and the
    multi-threaded metric sums nondeterministically; the thresholds are therefore bounds, not
    golden values.
    """
    pfx = str(out_prefix)
    mov = str(moving)
    fix = str(fixed)
    return [
        "antsRegistration",
        "--dimensionality",
        "3",
        "--output",
        f"[{pfx},{pfx}_Warped.nii.gz]",
        "--initial-moving-transform",
        f"[{fix},{mov},1]",
        "--transform",
        "Rigid[0.1]",
        "--metric",
        f"MI[{fix},{mov},1,32,Regular,0.25]",
        "--convergence",
        "[40x20x0,1e-5,4]",
        "--shrink-factors",
        "4x2x1",
        "--smoothing-sigmas",
        "1x0.5x0vox",
        "--random-seed",
        str(ANTS_RANDOM_SEED),
    ]


@pytest.mark.slow
@pytest.mark.skipif(
    not _has_ants_registration(),
    reason="antsRegistration not found (bundled or PATH)",
)
def test_synthetic_ct_anchor_ants_metrics(tmp_path: Path) -> None:
    """End-to-end: misaligned BLOBs → ANTs → warped ~ fixed in foreground (RMSE, correlation)."""
    shape = (64, 64, 64)
    aff = _eye_mm((2.0, 2.0, 2.0))
    fix_c = (32.0, 32.0, 32.0)
    mov_c = (32.0, 32.0, 28.0)
    r = 10.0
    fixed = _sphere_volume(shape, fix_c, r, 100.0, 0.0)
    moving = _sphere_volume(shape, mov_c, r, 100.0, 0.0)
    f_path = tmp_path / "fixed.nii.gz"
    m_path = tmp_path / "moving.nii.gz"
    nib.save(nib.Nifti1Image(fixed, aff), f_path)
    nib.save(nib.Nifti1Image(moving, aff), m_path)
    pfx = tmp_path / "out_ants"
    argv = _build_fast_ct_anchor_like_argv(m_path, f_path, pfx)
    ants_bin = _ants_registration_bin()
    assert ants_bin is not None
    argv[0] = ants_bin
    env = {**os.environ, "ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS": "2"}
    r = run_argv(
        argv,
        check=False,
        capture=True,
        env=env,
    )
    if r.returncode != 0:
        err = (r.stderr or r.stdout or b"")[:2000]
        if isinstance(err, bytes):
            err = err.decode("utf-8", errors="replace")
        pytest.fail(f"antsRegistration failed rc={r.returncode}: {err}")
    warped_p = tmp_path / "out_ants_Warped.nii.gz"
    assert warped_p.is_file()
    w = np.asanyarray(nib.load(warped_p).dataobj, dtype=np.float32)
    assert w.shape == shape
    assert np.isfinite(w).all(), "warped has NaN/Inf"
    assert float(w.max()) > 0.1, f"warped near zero max={w.max()}"
    mask = _sphere_shell_mask(shape, fix_c, 7.5, 11.0)
    assert int(np.count_nonzero(mask)) > 100
    rmse = float(np.sqrt(np.mean((w[mask] - fixed[mask]) ** 2)))
    assert rmse < _MAX_RMSE, f"RMSE(shell) {rmse} >= {_MAX_RMSE}"
    pc = _masked_pearson(w, fixed, mask)
    ncc = _masked_ncc(w, fixed, mask)
    assert pc >= _MIN_MASKED_PEARSON, f"masked Pearson {pc} < {_MIN_MASKED_PEARSON}"
    assert ncc >= _MIN_MASKED_NCC, f"masked NCC {ncc} < {_MIN_MASKED_NCC}"


def test_linear_argv_matches_production_kinds() -> None:
    """No binary: assert fast test argv has same *transform* names + MI as production builder."""
    m, f, p = "/tmp/mov.nii", "/tmp/fix.nii", "/tmp/p"
    a = _build_fast_ct_anchor_like_argv(Path(m), Path(f), Path(p))
    b = build_ants_registration_argv(m, f, p)

    def _kinds(arv: list[str]) -> list[str]:
        return [
            t for t in arv if t in ("Rigid[0.1]", "Affine[0.1]") or t.startswith("MI[")
        ]

    assert _kinds(a) == _kinds(b), (_kinds(a), _kinds(b))
    assert "Affine[0.1]" not in b
    assert (
        sum(1 for t in a if t.startswith("MI["))
        == sum(1 for t in b if t.startswith("MI["))
        == 1
    )


def test_production_seed_matches_this_module() -> None:
    b = build_ants_registration_argv("/tmp/mov.nii", "/tmp/fix.nii", "/tmp/p")
    assert b[b.index("--random-seed") + 1] == str(ANTS_RANDOM_SEED)


def _has_lead_tutor_15454_preproc(root: Path) -> bool:
    p = (
        root
        / "derivatives"
        / "leaddbs"
        / "sub-15454"
        / "preprocessing"
        / "anat"
        / "sub-15454_ses-preop_acq-iso_desc-preproc_T1w.nii.gz"
    )
    return p.is_file()


@pytest.mark.integration
@pytest.mark.skipif(
    not _has_ants_registration(),
    reason="antsRegistration not found (bundled or PATH)",
)
def test_lead_tutor_15454_anchor_n4_exists_and_loads(lead_tutor_root: Path) -> None:
    """If LeadTutor on disk, anchor N4 loads and has reasonable stats (regression on corrupt files)."""
    if not _has_lead_tutor_15454_preproc(lead_tutor_root):
        pytest.skip("sub-15454 preprocessing N4 not in tree")
    p = (
        lead_tutor_root
        / "derivatives"
        / "leaddbs"
        / "sub-15454"
        / "preprocessing"
        / "anat"
        / "sub-15454_ses-preop_acq-iso_desc-preproc_T1w.nii.gz"
    )
    d = np.asanyarray(nib.load(p).dataobj, dtype=np.float32)
    assert d.ndim == 3
    assert np.isfinite(d).all()
    assert float(d.max()) > float(d.min())
