# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
End-to-end on a synthetic head with ``--reg-backend wgpu`` (roadmap 3.18): ``preprocess`` (N4
bias-field correction), ``coreg`` (CT -> anchor multi-start rigid), ``normalize`` (SyN, T2w
coregistration, CT apply) and the native-to-template point warp of ``reconstruct`` run in-process
on the GPU, each checked against the same closed-form ground truth as ``test_e2e_synthetic.py``.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import numpy as np
import pytest
from dbsloc_bin import dbsloc_or_skip
from synthetic_head import EL_MODEL, write_synthetic_dataset

from dbsloc.bundled_bin import which_ants_binary
from dbsloc.reconstruction.io import SIDE_LEFT, SIDE_RIGHT

_NEEDS = ("antsRegistration", "N4BiasFieldCorrection")
_SPAN_MM = 9.0

pytestmark = [
    pytest.mark.slow,
    pytest.mark.skipif(
        any(which_ants_binary(b) is None for b in _NEEDS),
        reason="ANTs binaries not bundled or on PATH",
    ),
]


@pytest.fixture(scope="module", autouse=True)
def _require_gpu() -> None:
    hr = pytest.importorskip("dbsloc_reconstruct", reason="PyO3 extension not built")
    probe = getattr(hr, "gpu_probe", None)
    if probe is None or not bool(probe()["available"]):
        pytest.skip("no hardware GPU adapter for --reg-backend wgpu")


def _run(
    root: Path,
    tpl: Path,
    stages: str,
    backend: str,
    *,
    grid: str = "full",
    checkreg: bool = False,
) -> dict:
    env = dict(os.environ)
    env.setdefault("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "4")
    env["DBSLOC_OPEN_SCENE3D"] = "0"
    env["DBSLOC_NO_BRIDGE"] = "1"
    env["DBSLOC_HEADLESS"] = "1"
    env["DBSLOC_CHECKREG_SKIP_NO_DISPLAY"] = "1"
    env.pop("DBSLOC_TEMPLATE_T1", None)
    env.pop("DBSLOC_REG_BACKEND", None)
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
        "trac_core_native",
        "--norm-isotropic-mm",
        grid,
        "--reg-backend",
        backend,
        "--headless",
        "--no-index",
    ]
    if not checkreg:
        cmd.append("--no-checkreg")
    p = subprocess.run(
        cmd, capture_output=True, text=True, env=env, timeout=900, check=False
    )
    assert p.stdout.lstrip().startswith("{"), p.stdout + p.stderr
    return json.loads(p.stdout)


def test_synthetic_head_end_to_end_on_the_gpu(tmp_path: Path) -> None:
    tpl, head = write_synthetic_dataset(tmp_path)
    stages = "preprocess,coreg,normalize,reconstruct"
    checkreg = True
    sub = tmp_path / "derivatives" / "leaddbs" / "sub-01"
    out = _run(tmp_path, tpl, stages, "wgpu", grid="1.0", checkreg=False)
    results = {r["stage"]: r["result"] for r in out["results"]}
    for st in stages.split(","):
        assert results.get(st, "").startswith("ok:"), (st, results)

    pre = sub / "preprocessing" / "anat"
    n4_nii = pre / "sub-01_ses-preop_desc-preproc_T1w.nii.gz"
    assert n4_nii.is_file()
    n4_rec = json.loads(
        (pre / "sub-01_ses-preop_desc-preproc_T1w.json").read_text(encoding="utf-8")
    )
    assert n4_rec["reg_backend"] == "wgpu"
    assert n4_rec["reg_requested"] == "wgpu"
    assert n4_rec["reg_adapter"]["name"]
    assert n4_rec["n4"]["shrink_factor"] == 4
    assert len(n4_rec["levels"]) == 4
    assert not list(pre.glob(".partial-*"))

    coreg = sub / "coregistration"
    ct_image = "sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz"
    xfm = "sub-01_ses-postop_from-CT_to-anchorNative_mode-image_xfm"
    for f in [
        coreg / "anat" / ct_image,
        coreg / "transformations" / f"{xfm}.mat",
        coreg / "transformations" / f"{xfm}.h5",
        coreg
        / "transformations"
        / "sub-01_ses-postop_from-anchorNative_to-CT_mode-image_xfm.h5",
        coreg / "log" / "sub-01_ses-postop_desc-multistart_CT.json",
        coreg / "log" / "sub-01_desc-coregmethod.json",
    ]:
        assert f.is_file(), f
    crec = json.loads(
        (coreg / "log" / "sub-01_ses-postop_desc-multistart_CT.json").read_text(
            encoding="utf-8"
        )
    )
    assert crec["reg_backend"] == "wgpu"
    assert crec["reg_requested"] == "wgpu"
    assert crec["reg_adapter"]["name"]
    assert crec["starts"][0]["metric_backend"] == "wgpu"
    assert crec["reg_timings"]["coarse"]
    assert crec["reg_timings"]["full_levels"]
    coreg_log = json.loads(
        (coreg / "log" / "sub-01_desc-coregmethod.json").read_text(encoding="utf-8")
    )
    assert coreg_log["engine"] == "wgpu"
    assert coreg_log["approval"] == {"CT": 0}
    assert coreg_log["CT"]["backend"] == "wgpu"
    for d in (coreg / "anat", coreg / "transformations", coreg / "log"):
        assert not list(d.glob(".partial-*")), d

    norm_root = sub / "normalization"
    norm = norm_root / "anat"
    assert next(norm.glob("sub-01_ses-preop_space-*_desc-preproc_T1w.nii.gz")).is_file()
    assert next(norm.glob("sub-01_ses-postop_space-*_desc-preproc_CT.nii.gz")).is_file()
    assert next(
        (norm_root / "transformations").glob(
            "sub-01_from-anchorNative_to-*_mode-image_xfm.h5"
        )
    ).is_file()
    assert next(
        (norm_root / "transformations").glob(
            "sub-01_from-*_to-anchorNative_mode-image_xfm.h5"
        )
    ).is_file()
    rec = json.loads(
        (norm_root / "log" / "sub-01_desc-normmethod.json").read_text(encoding="utf-8")
    )
    assert rec["reg_backend"] == "wgpu"
    assert rec["reg_requested"] == "wgpu"
    assert rec["engine"] == "wgpu"
    assert rec["approval"] == 0
    assert rec["reg_adapter"]["name"]
    assert rec["norm_isotropic_mm"] == 1.0
    for d in (norm, norm_root / "transformations", norm_root / "log"):
        assert not list(d.glob(".partial-*")), d
    assert not (norm_root / "work").exists()
    if checkreg:
        again = _run(tmp_path, tpl, "normalize", "wgpu", grid="1.0", checkreg=True)
        verdict = {r["stage"]: r["result"] for r in again["results"]}
        assert verdict["normalize"].startswith("error: norm: qc not approved"), verdict
        pngs = {p.name for p in (norm_root / "checkreg").glob("*.png")}
        assert any(n.endswith("_desc-preproc_T1w.png") for n in pngs), pngs
        assert any(n.endswith("_desc-preproc_CT.png") for n in pngs), pngs

    reco = json.loads(
        next((sub / "reconstruction").glob("*_desc-reconstruction.json")).read_text(
            encoding="utf-8"
        )
    )
    assert reco["mni"][SIDE_RIGHT]["markers"]["head"][0] > 0
    assert reco["mni"][SIDE_LEFT]["markers"]["head"][0] < 0
    for key, c0 in head.lead_heads_canonical().items():
        u_c = head.lead_dirs_canonical()[key]
        mni = reco["mni"][key]["markers"]
        assert np.linalg.norm(np.asarray(mni["head"]) - c0) < 2.0
        tail_c = c0 + (_SPAN_MM / head.scale) * u_c
        assert np.linalg.norm(np.asarray(mni["tail"]) - tail_c) < 2.0


def test_an_approved_ants_result_is_rerun_for_an_explicit_gpu_request(
    tmp_path: Path,
) -> None:
    tpl, _head = write_synthetic_dataset(tmp_path)
    first = _run(tmp_path, tpl, "preprocess,coreg,normalize", "ants")
    results = {r["stage"]: r["result"] for r in first["results"]}
    assert results["normalize"].startswith("ok:"), results
    sub = tmp_path / "derivatives" / "leaddbs" / "sub-01"
    rec_path = sub / "normalization" / "log" / "sub-01_desc-normmethod.json"
    assert json.loads(rec_path.read_text(encoding="utf-8"))["reg_backend"] == "ants"
    approve = subprocess.run(
        [
            dbsloc_or_skip(),
            "lock",
            "approve",
            str(tmp_path),
            "01",
            "normalize",
            "--no-index",
        ],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert approve.returncode == 0, approve.stderr
    second = _run(tmp_path, tpl, "normalize", "wgpu")
    again = {r["stage"]: r["result"] for r in second["results"]}
    assert again["normalize"].startswith("ok:"), again
    assert json.loads(rec_path.read_text(encoding="utf-8"))["reg_backend"] == "wgpu"
