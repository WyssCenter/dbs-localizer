# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Reco v2 I/O and the Lead-Tutor chain on hand-built reconstructions (no tracing)."""

from __future__ import annotations

import numpy as np

from dbsloc.reconstruction.io import (
    SIDE_KEYS_CONVENTION,
    SIDE_LEFT,
    SIDE_NAMES,
    SIDE_RIGHT,
    RecoFileV2,
    read_recon_json_v2,
    write_recon_json_v2,
)


def _side(x0: float, pitch: float = 2.0) -> dict[str, object]:
    head = np.array([x0, -2.0, -6.0])
    u = np.array([0.0, 0.0, 1.0])
    coords = [head + k * pitch * u for k in range(4)]
    return {
        "coords_mm": [c.tolist() for c in coords],
        "trajectory": [(head + (z - 10) * u).tolist() for z in range(50)],
        "markers": {
            "head": head.tolist(),
            "tail": coords[-1].tolist(),
            "x": (head + np.array([0.635, 0.0, 0.0])).tolist(),
            "y": (head + np.array([0.0, 0.635, 0.0])).tolist(),
        },
        "trajvector": u.tolist(),
    }


def test_reco_v2_json_roundtrip(tmp_path) -> None:
    mni = {SIDE_RIGHT: _side(12.0), SIDE_LEFT: _side(-12.0)}
    r = RecoFileV2(subj_id="test", method="m", mni=mni, version=2)
    p = tmp_path / "a.json"
    write_recon_json_v2(p, r)
    r2 = read_recon_json_v2(p)
    assert r2.mni[SIDE_RIGHT]["markers"]["head"] == mni[SIDE_RIGHT]["markers"]["head"]
    assert r2.meta["tail_fiducial"] == "fourth_level"
    assert r.meta == {}


def test_side_keys_follow_specs_09_right_first() -> None:
    assert SIDE_NAMES[SIDE_RIGHT] == "right" and SIDE_RIGHT == "1"
    assert SIDE_NAMES[SIDE_LEFT] == "left" and SIDE_LEFT == "2"
    assert SIDE_KEYS_CONVENTION == "1=right,2=left"
