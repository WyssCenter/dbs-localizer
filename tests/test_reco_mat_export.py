# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""The reconstruction ``.mat`` export read back with scipy (``specs/12`` §8 checks 2, 4, 5).

The fixture ``tests/fixtures/reco_mat/bilateral.mat`` and its source ``bilateral.json`` are
written by the Rust test ``reco_mat::tests::reco_mat_bilateral_layout_classes_and_values``
with ``DBSLOC_RECO_MAT_FIXTURE_OUT=tests/fixtures/reco_mat``.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pytest

sio = pytest.importorskip("scipy.io", reason="scipy is not installed")

FIXTURES = Path(__file__).parent / "fixtures" / "reco_mat"
MODEL = "Boston Scientific Vercise Directed"
LEAD_RADIUS_MM = 0.65
MARKERS = ("head", "tail", "x", "y")


@pytest.fixture(scope="module")
def mat() -> dict:
    return sio.loadmat(
        FIXTURES / "bilateral.mat", struct_as_record=False, squeeze_me=False
    )


@pytest.fixture(scope="module")
def source() -> dict:
    return json.loads((FIXTURES / "bilateral.json").read_text(encoding="utf-8"))


def _only(arr: np.ndarray):
    assert arr.shape == (1, 1)
    return arr[0, 0]


def _text(arr: np.ndarray) -> str:
    assert arr.shape == (1,)
    return str(arr[0])


def test_file_holds_reco_and_the_provenance(mat: dict) -> None:
    names = sorted(k for k in mat if not k.startswith("__"))
    assert names == ["dbsloc_provenance", "reco"]
    assert mat["__version__"] == "1.0"
    prov = _only(mat["dbsloc_provenance"])
    assert _text(prov.source_json) == "sub-1_desc-reconstruction.json"
    assert _text(prov.side_keys) == "1=right,2=left"
    assert _text(prov.mni_source) == "mni"


def test_classes_and_shapes_match_specs_12(mat: dict) -> None:
    reco = _only(mat["reco"])
    assert sorted(reco._fieldnames) == ["mni", "native", "props"]
    props = reco.props
    assert props.shape == (1, 2)
    for k in range(2):
        assert _text(props[0, k].elmodel) == MODEL
        flag = props[0, k].manually_corrected
        assert flag.dtype == np.float64
        assert flag.shape == (1, 1)
    assert [props[0, k].manually_corrected[0, 0] for k in range(2)] == [1.0, 0.0]
    for name in ("native", "mni"):
        block = _only(getattr(reco, name))
        assert block.coords_mm.shape == (1, 2)
        assert block.trajectory.shape == (1, 2)
        assert block.markers.shape == (1, 2)
        for k in range(2):
            assert block.coords_mm[0, k].shape == (8, 3)
            assert block.coords_mm[0, k].dtype == np.float64
            assert block.trajectory[0, k].shape == (50, 3)
            for m in MARKERS:
                assert getattr(block.markers[0, k], m).shape == (1, 3)
    native = _only(reco.native)
    mni = _only(reco.mni)
    assert "trajvector" not in native._fieldnames
    assert mni.trajvector.shape == (1, 2)
    for k in range(2):
        head = mni.markers[0, k].head[0]
        tail = mni.markers[0, k].tail[0]
        u = (tail - head) / np.linalg.norm(tail - head)
        np.testing.assert_allclose(mni.trajvector[0, k][0], u, atol=1e-12)


@pytest.mark.parametrize("name", ["native", "mni"])
def test_values_round_trip_from_the_json(mat: dict, source: dict, name: str) -> None:
    block = _only(getattr(_only(mat["reco"]), name))
    for k, key in enumerate(("1", "2")):
        side = source[name][key]
        np.testing.assert_allclose(
            block.coords_mm[0, k], np.array(side["coords_mm"]), atol=1e-9
        )
        np.testing.assert_allclose(
            block.trajectory[0, k], np.array(side["trajectory"]), atol=1e-9
        )
        markers = block.markers[0, k]
        for m in MARKERS:
            np.testing.assert_allclose(
                getattr(markers, m)[0], np.array(side["markers"][m]), atol=1e-9
            )
        head = markers.head[0]
        for m in ("x", "y"):
            radius = np.linalg.norm(getattr(markers, m)[0] - head)
            assert abs(radius - LEAD_RADIUS_MM) < 1e-6


def test_side_one_is_the_right_hemisphere(mat: dict) -> None:
    mni = _only(_only(mat["reco"]).mni)
    assert mni.markers[0, 0].head[0, 0] > 0.0
    assert mni.markers[0, 1].head[0, 0] < 0.0
