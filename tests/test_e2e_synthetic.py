# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
End-to-end on a synthetic head with bundled ANTs (roadmap 4.4): preprocess, coreg, normalize
and reconstruct through ``dbsloc run``, each checked against closed-form ground truth.

Tests that reconstruct a CT share one preprocess / coreg / normalize run (module fixture) and
each reconstructs in its own copy of that tree. About a minute and a half on four ITK threads.
Runs the Rust ``dbsloc`` binary (``DBSLOC_BIN``, else ``target/release`` or ``target/debug``).
Skips without bundled or PATH ANTs or without the binary.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import nibabel as nib
import numpy as np
import pytest
from dbsloc_bin import dbsloc_or_skip
from synthetic_head import EL_MODEL, _labels, _world, write_synthetic_dataset

from dbsloc.bundled_bin import which_ants_binary
from dbsloc.reconstruction.io import SIDE_LEFT, SIDE_RIGHT

_NEEDS = ("antsRegistration", "antsApplyTransforms", "N4BiasFieldCorrection")
_SPAN_MM = 9.0
_MR_HEAD_TOL_MM = 4.5

pytestmark = [
    pytest.mark.slow,
    pytest.mark.skipif(
        any(which_ants_binary(b) is None for b in _NEEDS),
        reason="ANTs binaries not bundled or on PATH",
    ),
]


def _run(root: Path, tpl: Path, stages: str, recon: str = "trac_core_native") -> dict:
    env = dict(os.environ)
    env.setdefault("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "4")
    env["DBSLOC_OPEN_SCENE3D"] = "0"
    env["DBSLOC_NO_BRIDGE"] = "1"
    env.pop("DBSLOC_TEMPLATE_T1", None)
    cmd = [
        dbsloc_or_skip(),
        "run",
        str(root),
        "01",
        "--stages",
        stages,
        "--template",
        str(tpl),
        "--el-model",
        EL_MODEL,
        "--recon-method",
        recon,
        "--norm-isotropic-mm",
        "full",
        "--no-checkreg",
        "--headless",
        "--no-index",
    ]
    p = subprocess.run(
        cmd, capture_output=True, text=True, env=env, timeout=900, check=False
    )
    assert p.stdout.lstrip().startswith("{"), p.stdout + p.stderr
    return json.loads(p.stdout)


_PREPARED_STAGES = "preprocess,coreg,normalize"


@pytest.fixture(scope="module")
def prepared_ct(tmp_path_factory: pytest.TempPathFactory):
    root = tmp_path_factory.mktemp("prepared_ct")
    tpl, head = write_synthetic_dataset(root)
    out = _run(root, tpl, _PREPARED_STAGES)
    results = {r["stage"]: r["result"] for r in out["results"]}
    for st in _PREPARED_STAGES.split(","):
        assert results.get(st, "").startswith("ok:"), (st, results)
    return root, tpl, head, results


def _reconstruct_in_copy(
    prepared, tmp_path: Path, recon: str
) -> tuple[Path, Path, object, dict]:
    root, tpl, head, prepared_results = prepared
    work = tmp_path / "tree"
    shutil.copytree(root, work)
    out = _run(work, tpl, "reconstruct", recon=recon)
    results = {r["stage"]: r["result"] for r in out["results"]}
    results.update(
        {k: v for k, v in prepared_results.items() if k not in results},
    )
    assert results.get("reconstruct", "").startswith("ok:"), results
    return work, tpl, head, results


def test_synthetic_head_end_to_end(prepared_ct, tmp_path: Path) -> None:
    tmp_path, _tpl, head, results = _reconstruct_in_copy(
        prepared_ct, tmp_path, "trac_core_native"
    )
    for st in ("preprocess", "coreg", "normalize", "reconstruct"):
        assert results.get(st, "").startswith("ok:"), (st, results)

    sub = tmp_path / "derivatives" / "leaddbs" / "sub-01"
    norm = sub / "normalization"
    warped = next((norm / "anat").glob("*_space-*_desc-preproc_T1w.nii.gz"))
    assert warped.is_file()
    assert next((norm / "anat").glob("*_space-*_desc-preproc_CT.nii.gz")).is_file()
    assert next(
        (norm / "transformations").glob(
            "sub-01_from-anchorNative_to-*_mode-image_xfm.h5"
        )
    ).is_file()
    assert next(
        (norm / "transformations").glob(
            "sub-01_from-*_to-anchorNative_mode-image_xfm.h5"
        )
    ).is_file()
    rec = json.loads(
        (norm / "log" / "sub-01_desc-normmethod.json").read_text(encoding="utf-8")
    )
    assert rec["norm_isotropic_mm"] is None
    assert rec["approval"] == 0
    assert rec["engine"] in ("ants", "wgpu")
    assert rec["custom"] is False
    assert not (norm / "work").exists()
    assert not list((norm / "anat").glob("_resampled_is*"))
    coreg_log = json.loads(
        (sub / "coregistration" / "log" / "sub-01_desc-coregmethod.json").read_text(
            encoding="utf-8"
        )
    )
    assert coreg_log["approval"] == {"CT": 0}
    assert coreg_log["CT"]["transform"] == (
        "sub-01_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat"
    )

    reco = json.loads(
        next((sub / "reconstruction").glob("*_desc-reconstruction.json")).read_text(
            encoding="utf-8"
        )
    )
    assert reco["meta"]["side_keys"] == "1=right,2=left"
    assert reco["mni"][SIDE_RIGHT]["markers"]["head"][0] > 0
    assert reco["mni"][SIDE_LEFT]["markers"]["head"][0] < 0
    for key, c0 in head.lead_heads_canonical().items():
        u_c = head.lead_dirs_canonical()[key]
        u_a = u_c @ head.anchor_r.T
        h_a = head.canonical_to_anchor(c0)
        nat = reco["native"][key]["markers"]
        assert np.linalg.norm(np.asarray(nat["head"]) - h_a) < 1.0
        assert np.linalg.norm(np.asarray(nat["tail"]) - (h_a + _SPAN_MM * u_a)) < 1.0
        mni = reco["mni"][key]["markers"]
        assert np.linalg.norm(np.asarray(mni["head"]) - c0) < 2.0
        tail_c = c0 + (_SPAN_MM / head.scale) * u_c
        assert np.linalg.norm(np.asarray(mni["tail"]) - tail_c) < 2.0


def _trilinear(vol: np.ndarray, ijk: np.ndarray) -> np.ndarray:
    f = np.floor(ijk).astype(int)
    w = ijk - f
    out = np.zeros(ijk.shape[:-1])
    for corner in range(8):
        off = np.array([(corner >> a) & 1 for a in range(3)])
        idx = f + off
        weight = np.prod(np.where(off == 1, w, 1.0 - w), axis=-1)
        inside = np.all((idx >= 0) & (idx < np.array(vol.shape)), axis=-1)
        safe = np.where(inside[..., None], idx, 0)
        vals = vol[safe[..., 0], safe[..., 1], safe[..., 2]]
        out += np.where(inside, weight * vals, 0.0)
    return out


def _assert_precoreg_matches_ants_apply(sub: Path, tpl: Path, tmp_path: Path) -> None:
    moving = sub / "preprocessing" / "anat" / "sub-01_ses-preop_desc-preproc_T1w.nii.gz"
    image = (
        sub / "coregistration" / "anat" / "sub-01_ses-preop_desc-precoreg_T1w.nii.gz"
    )
    matrix = next(
        (sub / "coregistration" / "transformations").glob(
            "sub-01_ses-preop_from-T1w_to-*_desc-precoreg_mode-image_xfm.mat"
        )
    )
    record = json.loads(
        (sub / "coregistration" / "log" / "sub-01_desc-precoreg.json").read_text(
            encoding="utf-8"
        )
    )
    assert record["backend"] in ("ants", "wgpu")
    assert np.asarray(record["matrix_ras"]).shape == (4, 4)

    mov_img = nib.load(str(moving))
    pre_img = nib.load(str(image))
    assert np.array_equal(np.asarray(mov_img.dataobj), np.asarray(pre_img.dataobj)), (
        "precoreg must not change a voxel value"
    )
    rot = pre_img.affine[:3, :3] @ np.linalg.inv(mov_img.affine[:3, :3])
    assert np.abs(rot @ rot.T - np.eye(3)).max() < 1e-6

    tpl_img = nib.load(str(tpl))
    shape = tpl_img.shape
    ijk = np.stack(np.meshgrid(*[np.arange(n) for n in shape], indexing="ij"), axis=-1)
    world = ijk @ tpl_img.affine[:3, :3].T + tpl_img.affine[:3, 3]
    pre_ijk = (world - pre_img.affine[:3, 3]) @ np.linalg.inv(pre_img.affine[:3, :3]).T
    sampled = _trilinear(np.asarray(pre_img.dataobj, dtype=np.float64), pre_ijk)

    warped = tmp_path / "ants_applied.nii.gz"
    apply_bin = which_ants_binary("antsApplyTransforms")
    assert apply_bin is not None
    env = dict(os.environ)
    env.setdefault("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "4")
    subprocess.run(
        [
            str(apply_bin),
            "-d",
            "3",
            "-i",
            str(moving),
            "-r",
            str(tpl),
            "-o",
            str(warped),
            "-n",
            "Linear",
            "-t",
            str(matrix),
        ],
        check=True,
        capture_output=True,
        env=env,
        timeout=300,
    )
    reference = np.asarray(nib.load(str(warped)).dataobj, dtype=np.float64)
    lo, hi = np.percentile(reference, [1, 99])
    rms = float(np.sqrt(np.mean((sampled - reference) ** 2)))
    assert rms < 0.01 * (hi - lo), (rms, lo, hi)


def test_synthetic_head_post_op_mr_end_to_end(tmp_path: Path) -> None:
    tpl, head = write_synthetic_dataset(tmp_path, postop="mr")
    stages = "preprocess,precoreg,coreg,normalize,reconstruct"
    out = _run(tmp_path, tpl, stages, recon="auto")
    results = {r["stage"]: r["result"] for r in out["results"]}
    for st in stages.split(","):
        assert results.get(st, "").startswith("ok:"), (st, results)
    assert results["precoreg"] == "ok:precoreg:sub-01_ses-preop_desc-precoreg_T1w"
    assert results["coreg"] == (
        "ok:coreg:sub-01_ses-postop_acq-ax_space-anchorNative_desc-preproc_T1w"
    )

    sub = tmp_path / "derivatives" / "leaddbs" / "sub-01"
    _assert_precoreg_matches_ants_apply(sub, tpl, tmp_path)

    coreg = sub / "coregistration" / "anat"
    assert (
        sub
        / "coregistration"
        / "transformations"
        / "sub-01_ses-postop_acq-ax_from-T1w_to-anchorNative_mode-image_xfm.mat"
    ).is_file()
    anchor = np.asarray(
        nib.load(str(coreg / "sub-01_ses-preop_desc-precoreg_T1w.nii.gz")).dataobj,
        dtype=np.float64,
    )
    warped = np.asarray(
        nib.load(
            str(
                coreg
                / "sub-01_ses-postop_acq-ax_space-anchorNative_desc-preproc_T1w.nii.gz"
            )
        ).dataobj,
        dtype=np.float64,
    )
    inside = (anchor > 1.0) & (warped > 1.0)
    assert inside.sum() > 1000
    corr = float(np.corrcoef(anchor[inside], warped[inside])[0, 1])
    assert corr > 0.9, corr

    norm = sub / "normalization" / "anat"
    mr_norm = next(
        norm.glob("sub-01_ses-postop_acq-ax_space-*_desc-preproc_T1w.nii.gz")
    )
    assert mr_norm.is_file()
    assert not list(norm.glob("*_CT.nii.gz"))
    reco = json.loads(
        next((sub / "reconstruction").glob("*_desc-reconstruction.json")).read_text(
            encoding="utf-8"
        )
    )
    assert [Path(p).name for p in reco["meta"]["norm_inputs"]] == [mr_norm.name]
    for key, c0 in head.lead_heads_canonical().items():
        mni = reco["mni"][key]["markers"]
        assert np.linalg.norm(np.asarray(mni["head"]) - c0) < _MR_HEAD_TOL_MM


def _tissue_coverage(
    mask_path: Path, image_path: Path, head, to_canonical
) -> tuple[float, float]:
    mask_img = nib.load(str(mask_path))
    image = nib.load(str(image_path))
    assert mask_img.shape == image.shape
    assert np.allclose(mask_img.affine, image.affine, atol=1e-3)
    mask = np.asarray(mask_img.dataobj) > 0
    tissue = _labels(to_canonical(_world(image.shape, image.affine)))
    brain = tissue["brain"] | tissue["stem"]
    return float(mask[brain].mean()), float(mask[tissue["skull"]].mean())


def test_synthetic_head_pacer_gets_the_template_brain_mask_on_the_ct_grid(
    prepared_ct, tmp_path: Path
) -> None:
    tmp_path, tpl, head, results = _reconstruct_in_copy(prepared_ct, tmp_path, "pacer")
    assert tpl.with_name("brainmask.nii.gz").is_file()
    for st in ("preprocess", "coreg", "normalize"):
        assert results.get(st, "").startswith("ok:"), (st, results)

    sub = tmp_path / "derivatives" / "leaddbs" / "sub-01"
    raw_ct = (
        tmp_path
        / "rawdata"
        / "sub-01"
        / "ses-postop"
        / "anat"
        / "sub-01_ses-postop_CT.nii.gz"
    )
    ct_mask = (
        sub / "reconstruction" / "sub-01_ses-postop_space-CT_desc-brain_mask.nii.gz"
    )
    anchor_mask = (
        sub
        / "reconstruction"
        / "sub-01_ses-postop_space-anchorNative_desc-brain_mask.nii.gz"
    )
    assert ct_mask.is_file() and anchor_mask.is_file()
    assert not list((sub / "reconstruction").glob(".partial-*"))
    covered, skull = _tissue_coverage(ct_mask, raw_ct, head, head.ct_to_canonical)
    assert covered >= 0.95, covered
    assert skull < 0.05, skull
    anchor = (
        tmp_path
        / "rawdata"
        / "sub-01"
        / "ses-preop"
        / "anat"
        / "sub-01_ses-preop_T1w.nii.gz"
    )
    covered, skull = _tissue_coverage(
        anchor_mask, anchor, head, head.anchor_to_canonical
    )
    assert covered >= 0.95, covered
    assert skull < 0.05, skull

    assert results.get("reconstruct", "").startswith("ok:"), results
    reco = json.loads(
        next((sub / "reconstruction").glob("*_desc-reconstruction.json")).read_text(
            encoding="utf-8"
        )
    )
    assert reco["meta"]["pacer_mask"] == "template"


def test_synthetic_head_ct_and_mr_are_both_coregistered_and_normalized(
    tmp_path: Path,
) -> None:
    tpl, _ = write_synthetic_dataset(tmp_path, postop="both")
    stages = "preprocess,coreg,normalize"
    out = _run(tmp_path, tpl, stages)
    results = {r["stage"]: r["result"] for r in out["results"]}
    for st in stages.split(","):
        assert results.get(st, "").startswith("ok:"), (st, results)
    assert results["coreg"] == (
        "ok:coreg:sub-01_ses-postop_space-anchorNative_desc-preproc_CT"
    )

    sub = tmp_path / "derivatives" / "leaddbs" / "sub-01"
    coreg = sub / "coregistration"
    for name in (
        "anat/sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz",
        "anat/sub-01_ses-postop_acq-ax_space-anchorNative_desc-preproc_T1w.nii.gz",
        "transformations/sub-01_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat",
        "transformations/sub-01_ses-postop_acq-ax_from-T1w_to-anchorNative_mode-image_xfm.mat",
    ):
        assert (coreg / name).is_file(), name
    coreg_log = json.loads(
        (coreg / "log" / "sub-01_desc-coregmethod.json").read_text(encoding="utf-8")
    )
    assert coreg_log["approval"] == {"CT": 0, "ax_MRI": 0}
    norm = sub / "normalization" / "anat"
    assert next(norm.glob("sub-01_ses-postop_space-*_desc-preproc_CT.nii.gz")).is_file()
    assert next(
        norm.glob("sub-01_ses-postop_acq-ax_space-*_desc-preproc_T1w.nii.gz")
    ).is_file()
