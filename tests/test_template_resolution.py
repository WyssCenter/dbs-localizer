# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Template discovery for normalize (roadmap 4.3, ``specs/04`` §3.1), through ``dbsloc run``."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest
from dbsloc_bin import dbsloc_or_skip

TEMPLATE_ENV = "DBSLOC_TEMPLATE_T1"


def _dataset(tmp_path: Path) -> Path:
    root = tmp_path / "ds"
    for ses, mod in (("preop", "T1w"), ("postop", "CT")):
        d = root / "rawdata" / "sub-01" / f"ses-{ses}" / "anat"
        d.mkdir(parents=True)
        (d / f"sub-01_ses-{ses}_{mod}.nii").write_bytes(b"x")
    (root / "derivatives" / "leaddbs" / "sub-01").mkdir(parents=True)
    return root


def _run(root: Path, env: dict[str, str], *extra: str) -> tuple[int, dict, str]:
    p = subprocess.run(
        [
            dbsloc_or_skip(),
            "run",
            str(root),
            "01",
            "--no-index",
            "--stages",
            "normalize",
            "--no-checkreg",
            "--headless",
            "--log",
            "quiet",
            *extra,
        ],
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
        check=False,
    )
    return p.returncode, (json.loads(p.stdout) if p.stdout.strip() else {}), p.stderr


def test_plan_and_normalize_with_stale_template_env(tmp_path: Path) -> None:
    root = _dataset(tmp_path)
    env = dict(os.environ)
    env[TEMPLATE_ENV] = str(tmp_path / "stale.nii")
    env["DBSLOC_OPEN_SCENE3D"] = "0"
    code, out, err = _run(root, env, "--plan")
    assert code == 0, err
    assert out["results"][0]["result"].startswith("plan:")
    code, out, err = _run(root, env)
    res = out["results"][0]["result"]
    assert code == 1
    assert "stale.nii does not exist" in res and "never replaced" in res, (res, err)


def test_explicit_template_wins_over_the_environment(tmp_path: Path) -> None:
    root = _dataset(tmp_path)
    env = dict(os.environ)
    env[TEMPLATE_ENV] = str(tmp_path / "env.nii")
    _code, out, err = _run(root, env, "--template", str(tmp_path / "flag.nii"))
    res = out["results"][0]["result"]
    assert "flag.nii does not exist" in res, (res, err)


TEMPLATE_T2_ENV = "DBSLOC_TEMPLATE_T2"


def _t1_t2_dataset(tmp_path: Path) -> Path:
    root = tmp_path / "ds2"
    d = root / "rawdata" / "sub-01" / "ses-preop" / "anat"
    d.mkdir(parents=True)
    for mod in ("T1w", "T2w"):
        (d / f"sub-01_ses-preop_{mod}.nii").write_bytes(b"x")
    return root


def test_t2_template_precedence_flag_env_sibling(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    hr = pytest.importorskip("dbsloc_reconstruct", reason="PyO3 extension not built")

    root = _t1_t2_dataset(tmp_path)
    tpl_dir = tmp_path / "tpl"
    tpl_dir.mkdir()
    (tpl_dir / "t1.nii").write_bytes(b"x")
    monkeypatch.delenv(TEMPLATE_T2_ENV, raising=False)
    opts = {
        "bids_dir": str(root),
        "subject": "01",
        "use_index": False,
        "normalization_template": str(tpl_dir / "t1.nii"),
    }

    def plan(**extra: object) -> dict:
        return json.loads(hr.plan_run(json.dumps({**opts, **extra})))

    assert plan()["template_t2"] is None
    assert plan()["norm_multispectral"] is True
    (tpl_dir / "t2.nii").write_bytes(b"x")
    assert Path(plan()["template_t2"]).name == "t2.nii"
    monkeypatch.setenv(TEMPLATE_T2_ENV, str(tmp_path / "env_t2.nii"))
    assert Path(plan()["template_t2"]).name == "env_t2.nii"
    got = plan(normalization_template_t2=str(tmp_path / "flag_t2.nii"))
    assert Path(got["template_t2"]).name == "flag_t2.nii"
    assert plan(norm_multispectral=False)["norm_multispectral"] is False


def test_missing_explicit_t2_template_fails_normalize_before_ants(
    tmp_path: Path,
) -> None:
    root = _t1_t2_dataset(tmp_path)
    t1 = tmp_path / "t1.nii"
    t1.write_bytes(b"x")
    env = dict(os.environ)
    env.pop(TEMPLATE_T2_ENV, None)
    env["DBSLOC_OPEN_SCENE3D"] = "0"
    _code, out, err = _run(
        root, env, "--template", str(t1), "--template-t2", str(tmp_path / "nope_t2.nii")
    )
    res = out["results"][0]["result"]
    if "antsRegistration not found" in res:
        pytest.skip("antsRegistration not available")
    assert "nope_t2.nii does not exist" in res and "--no-norm-multispectral" in res, (
        res,
        err,
    )
