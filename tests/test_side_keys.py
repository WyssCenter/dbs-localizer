# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Hemisphere resolution for Reco v2 files of both side-key vintages (roadmap 2.18)."""

from __future__ import annotations

import pytest

from dbsloc.reconstruction.io import (
    SIDE_KEYS_CONVENTION,
    RecoFileV2,
    parse_hemisphere,
    side_hemispheres,
    side_key_for,
)


def _side(x: float) -> dict:
    return {
        "markers": {
            "head": [x, -10.0, -8.0],
            "tail": [x + 1.0, -6.0, -1.0],
            "x": [x + 0.6, -10.0, -8.0],
        },
        "trajectory": [[x, -10.0, -8.0], [x + 1.0, -6.0, -1.0]],
    }


def _new() -> RecoFileV2:
    return RecoFileV2(
        subj_id="01",
        method="t",
        mni={"1": _side(12.0), "2": _side(-12.0)},
        meta={"side_keys": SIDE_KEYS_CONVENTION},
    )


def _pre_flip() -> RecoFileV2:
    return RecoFileV2(
        subj_id="01", method="t", mni={"1": _side(-12.0), "2": _side(12.0)}
    )


def test_declared_convention_is_read_by_key() -> None:
    reco = _new()
    assert side_hemispheres(reco) == {"1": "right", "2": "left"}
    assert side_key_for(reco, "1") == "1"
    assert side_key_for(reco, "left") == "2"


def test_pre_flip_file_is_read_by_x() -> None:
    reco = _pre_flip()
    assert side_hemispheres(reco) == {"1": "left", "2": "right"}
    assert side_key_for(reco, "1") == "2"
    assert reco.mni[side_key_for(reco, "right")]["markers"]["head"][0] > 0


def test_single_side_without_declaration_goes_by_sign() -> None:
    reco = RecoFileV2(subj_id="01", method="t", mni={"1": _side(-12.0)})
    assert side_hemispheres(reco) == {"1": "left"}
    with pytest.raises(ValueError, match="no right side"):
        side_key_for(reco, "right")


def test_unknown_convention_and_bad_side_are_errors() -> None:
    reco = _new()
    reco.meta["side_keys"] = "1=left,2=right"
    with pytest.raises(ValueError, match="not a known convention"):
        side_hemispheres(reco)
    with pytest.raises(ValueError):
        parse_hemisphere("3")
