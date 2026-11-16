# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""benchmarks.accuracy end to end on a synthetic two-subject dataset: run (FastWarp and ANTs),
evaluate, report. Needs a GPU and ANTsPy; skips otherwise. No download, no real data."""

from __future__ import annotations

import copy
import csv
import json

import fastwarp as g
import numpy as np
import pytest
from test_register import SPACING, needs_gpu, phantom

from benchmarks.accuracy import datasets, evaluate, linear, post, recipes, report, run

ants = pytest.importorskip("ants")

SHIFT = (4.0, -3.0, 2.0)
LEVELS = {"shrink_factors": [2, 1], "smoothing_sigmas": [1.0, 0.0]}


def _tiny_recipe() -> dict:
    r = copy.deepcopy(recipes.RECIPES["cohort"])
    for stage in ("rigid", "affine"):
        r[stage].update(iterations=[40, 20], **LEVELS)
    r["syn"].update(iterations=[10, 5], **LEVELS)
    return r


def _subject(root, sid: str, shift) -> datasets.Subject:
    vol, sp, origin, _ = phantom(shift)
    img = ants.from_numpy(vol.astype("float32"), spacing=sp, origin=origin)
    pts = np.indices(vol.shape, dtype=np.float64) * SPACING
    c = np.array([56.0, 40.0, 40.0]) + shift
    nucleus = sum(((pts[a] - c[a]) / (8, 6, 6)[a]) ** 2 for a in range(3)) <= 1.0
    brain = vol > 0.5
    labels = np.where(nucleus, 10, np.where(brain, 1002, 0)).astype("float32")
    folder = root / sid
    folder.mkdir(parents=True)
    ants.image_write(img, str(folder / "t1w.nii.gz"))
    ants.image_write(img.new_image_like(labels), str(folder / "labels.nii.gz"))
    ants.image_write(img.new_image_like(nucleus.astype("float32")), str(folder / "nucleus.nii.gz"))
    ras = [(-c[0], -c[1], c[2]), (-(c[0] + 20), -(c[1] + 30), c[2] + 5)]
    rows = "".join(
        f"n{i},{x},{y},{z},0,0,0,1,1,1,1,{i},P{i},vol\n" for i, (x, y, z) in enumerate(ras, 1)
    )
    (folder / "points.fcsv").write_text(
        "# Markups fiducial file version = 4.6\n# CoordinateSystem = 0\n"
        "# columns = id,x,y,z,ow,ox,oy,oz,vis,sel,lock,label,desc,associatedNodeID\n" + rows
    )
    return datasets.Subject(
        id=sid,
        image=f"{sid}/t1w.nii.gz",
        labels=f"{sid}/labels.nii.gz",
        landmarks=f"{sid}/points.fcsv",
        masks=((7, f"{sid}/nucleus.nii.gz"),),
    )


@needs_gpu
def test_run_evaluate_report_on_a_phantom(tmp_path, monkeypatch):
    data, work = tmp_path / "data", tmp_path / "work"
    root = data / "smoke"
    subjects = (_subject(root, "s-1", (0.0, 0.0, 0.0)), _subject(root, "s-2", SHIFT))
    (root / ".fetched.json").write_text("{}")
    ds = datasets.Dataset(
        key="smoke",
        title="phantom",
        licence="",
        downloads=(),
        subjects=subjects,
        protocol="pairwise",
        groups={"subcortical": (10, 7), "cortical": (1002,)},
        label_names={10: "nucleus", 7: "nucleus mask"},
    )
    monkeypatch.setattr(datasets, "registry", lambda: {"smoke": ds})
    monkeypatch.setitem(recipes.RECIPES, "tiny", _tiny_recipe())
    monkeypatch.setenv("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "1")

    kw = {"data": data, "work": work, "keys": ["smoke"], "recipe_name": "tiny", "pairs": 1}
    rows = run.run(**kw, quick=False, iso_mm=SPACING, threads=1, log=lambda s: None)
    assert [(r["engine"], r["status"], r["ran"]) for r in rows] == [
        ("fastwarp", "ok", True),
        ("ants", "ok", True),
    ]
    again = run.run(**kw, quick=False, iso_mm=SPACING, threads=1, log=lambda s: None)
    assert not any(r["ran"] for r in again)

    pair = work / "smoke" / "tiny" / "s-1__s-2"
    ants_run = json.loads((pair / "ants" / "run.json").read_text())
    assert ants_run["argv"][ants_run["argv"].index("--convergence") + 1] == "[40x20,1e-06,10]"
    fw_run = json.loads((pair / "fastwarp" / "run.json").read_text())
    assert fw_run["recipe_matches"] and fw_run["environment"]["fastwarp"] == g.__version__
    assert fw_run["timings"]["total_s"] > 0 and fw_run["timings"]["syn_s"] > 0
    assert ants_run["timings"]["linear_s"] > 0 and ants_run["timings"]["syn_s"] > 0
    assert "note" not in ants_run["timings"]

    lin = linear.run_linear(**kw, quick=False, iso_mm=SPACING, threads=1, log=lambda s: None)
    assert [(r["engine"], r["status"]) for r in lin] == [("fastwarp", "ok"), ("ants", "ok")]
    comparison = json.loads(
        (work / "smoke" / "linear" / "s-1__s-2" / "comparison.json").read_text()
    )
    for fit in ("rigid", "affine"):
        assert comparison["fits"][fit]["fastwarp"]["landmark_mean_mm"] < 3.0
        assert comparison["fits"][fit]["agreement"]["mean_mm"] < 1.0
    posted = post.run_post(**kw, quick=False, iso_mm=SPACING, threads=1, log=lambda s: None)
    assert [r["status"] for r in posted] == ["ok"]
    post_record = json.loads((pair / "post.json").read_text())
    assert post_record["agreement"]["forward_field"]["field_diff_max_mm"] < 0.01
    assert not (pair / "post_tmp").exists()

    out = evaluate.evaluate(**kw, quick=False, log=lambda s: None)
    with open(out / "pairs.csv", newline="") as f:
        pairs = list(csv.DictReader(f))
    assert len(pairs) == 2
    for r in pairs:
        assert float(r["dice_subcortical"]) > 0.7, r
        assert float(r["landmark_mean_mm"]) < 2.0, r
        assert float(r["jac_nonpositive_frac"]) == 0.0, r
    with open(out / "labels.csv", newline="") as f:
        assert {r["label"] for r in csv.DictReader(f)} == {"7", "10", "1002"}
    with open(out / "agreement.csv", newline="") as f:
        agreement = list(csv.DictReader(f))
    assert len(agreement) == 1 and float(agreement[0]["field_diff_rms_mm"]) < 2.0

    md = report.report(work=work, recipe_name="tiny", plot=False)
    text = md.read_text()
    assert "FastWarp against ANTs" in text and "dice_subcortical" in text
    assert "## Registration time" in text and "## Linear fits" in text
    assert "## Post-processing" in text
    assert (out / "paired.csv").is_file() and (out / "structures.csv").is_file()


def test_the_moving_image_goes_onto_the_registration_grid(tmp_path, monkeypatch):
    root = tmp_path / "data" / "m"
    root.mkdir(parents=True)
    vol = np.random.default_rng(0).random((20, 16, 12)).astype("float32")
    img = ants.from_numpy(vol, spacing=(1.5, 1.5, 1.5), origin=(3.0, -2.0, 1.0))
    ants.image_write(img, str(root / "a.nii.gz"))
    ds = datasets.Dataset(
        key="m",
        title="m",
        licence="",
        downloads=(),
        subjects=(datasets.Subject(id="a", image="a.nii.gz"),),
        protocol="pairwise",
    )
    work = tmp_path / "work"
    out = run.prepare_moving(ds, "a", tmp_path / "data", work, 2.0)
    moved = ants.image_read(str(out))
    assert out.parent == run.prep_folder(ds, "a", work, 2.0)
    np.testing.assert_allclose(moved.spacing, (2.0, 2.0, 2.0))
    np.testing.assert_allclose(moved.origin, img.origin)
    assert run.prepare_moving(ds, "a", tmp_path / "data", work, 1.5) == root / "a.nii.gz"
