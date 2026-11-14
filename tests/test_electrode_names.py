# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

from __future__ import annotations

import dataclasses
import math
from typing import Final

import pytest

from dbsloc.electrodes.layout import layout_from_spec
from dbsloc.electrodes.names import (
    NAMING_RULES,
    SIDES,
    UnspecifiedNaming,
    contact_names,
    level_names,
    naming_rule,
)
from dbsloc.electrodes.registry import get_elspec, list_models


def _names(model: str, side: str) -> list[str]:
    return list(contact_names(get_elspec(model), side))  # type: ignore[arg-type]


def _levels(model: str, side: str) -> list[str]:
    return list(level_names(get_elspec(model), side))  # type: ignore[arg-type]


def test_every_catalogue_model_has_a_rule() -> None:
    catalogue = {matfname for _, matfname in list_models()}
    assert catalogue == set(NAMING_RULES)
    for name, _ in list_models():
        spec = get_elspec(name)
        for side in SIDES:
            assert len(contact_names(spec, side)) == spec.numContacts, name
            assert len(level_names(spec, side)) == len(spec.etageidx), name


def test_medtronic_ring_leads_start_at_zero_and_eight() -> None:
    assert _names("Medtronic 3389", "1") == ["K0", "K1", "K2", "K3"]
    assert _names("Medtronic 3389", "2") == ["K8", "K9", "K10", "K11"]
    assert _levels("Medtronic 3389", "2") == ["K8", "K9", "K10", "K11"]
    assert _names("PINS Medical L302", "2")[0] == "K8"
    assert _names("NeuroPace DL-344-3.5", "1")[0] == "K0"


def test_medtronic_directional_uses_level_and_letter() -> None:
    want = ["K0", "K1A", "K1B", "K1C", "K2A", "K2B", "K2C", "K3"]
    for side in SIDES:
        assert _names("Medtronic B33005", side) == want
        assert _names("Medtronic B33015", side) == want
    assert _levels("Medtronic B33005", "1") == ["K0", "K1", "K2", "K3"]


def test_boston_leads_number_segments() -> None:
    assert _names("Boston Scientific Vercise Directed", "1") == [
        f"K{i}" for i in range(9, 17)
    ]
    assert _names("Boston Scientific Vercise Directed", "2")[0] == "K1"
    assert _levels("Boston Scientific Vercise Directed", "1") == [
        "K9",
        "K10-12",
        "K13-15",
        "K16",
    ]
    assert _names("Boston Scientific Vercise", "1")[0] == "K9"
    assert _names("Boston Scientific Vercise Cartesia HX", "1")[0] == "K17"
    assert _names("Boston Scientific Vercise Cartesia HX", "1")[-1] == "K32"
    assert _names("Boston Scientific Vercise Cartesia X", "2")[0] == "K1"
    assert _levels("Boston Scientific Vercise Cartesia X", "2") == [
        "K1-3",
        "K4-6",
        "K7-9",
        "K10-12",
        "K13-15",
        "K16",
    ]


def test_abbott_leads() -> None:
    assert _names("Abbott ActiveTip (6142-6145)", "2") == ["K1", "K2", "K3", "K4"]
    want = ["K1", "K2A", "K2B", "K2C", "K3A", "K3B", "K3C", "K4"]
    assert _names("Abbott Directed 6172 (short)", "1") == want
    assert _names("Abbott Directed 6173 (long)", "2") == want
    assert _levels("Abbott Directed 6172 (short)", "2") == ["K1", "K2", "K3", "K4"]


def _azimuth_deg(xyz: tuple[float, float, float]) -> float:
    return round(math.degrees(math.atan2(xyz[1], xyz[0])) % 360.0, 6)


_CCW: Final[tuple[float, float, float]] = (90.0, 210.0, 330.0)
_CW: Final[tuple[float, float, float]] = (90.0, 330.0, 210.0)


@pytest.mark.parametrize(
    ("model", "a_b_c"),
    [
        ("Medtronic B33005", _CCW),
        ("Medtronic B33015", _CCW),
        ("Abbott Directed 6172 (short)", _CW),
        ("Abbott Directed 6173 (long)", _CW),
    ],
)
def test_segment_letters_match_the_segment_azimuths(
    model: str, a_b_c: tuple[float, float, float]
) -> None:
    spec = get_elspec(model)
    layout = layout_from_spec(spec)
    want = dict(zip("ABC", a_b_c, strict=True))
    seen = 0
    for side in SIDES:
        for i, name in enumerate(contact_names(spec, side)):
            if name[-1] in want:
                assert _azimuth_deg(layout.contact_xyz_mm[i]) == want[name[-1]], name
                seen += 1
    assert seen == 2 * 6


@pytest.mark.parametrize(
    ("model", "a_b_c"),
    [
        ("Boston Scientific Vercise Directed", _CCW),
        ("Boston Scientific Vercise Cartesia HX", _CCW),
        ("Boston Scientific Vercise Cartesia X", _CCW),
        ("Aleva directSTIM Directed", _CW),
    ],
)
def test_numbered_segments_run_a_b_c_in_ascending_names(
    model: str, a_b_c: tuple[float, float, float]
) -> None:
    spec = get_elspec(model)
    layout = layout_from_spec(spec)
    seen = 0
    for side in SIDES:
        names = contact_names(spec, side)
        for level in spec.etageidx:
            if len(level) != 3:
                continue
            ordered = sorted(level, key=lambda c: int(names[c - 1][1:]))
            got = tuple(_azimuth_deg(layout.contact_xyz_mm[c - 1]) for c in ordered)
            assert got == a_b_c, (side, [names[c - 1] for c in ordered])
            seen += 1
    assert seen >= 2


def test_sceneray_aleva_and_small_leads() -> None:
    assert _names("SceneRay SR1200", "1") == ["K4", "K5", "K6", "K7"]
    assert _names("SceneRay SR1200", "2") == ["K0", "K1", "K2", "K3"]
    assert _names("SceneRay SR1202", "1")[0] == "K8"
    assert _names("SceneRay SR1212", "2")[0] == "K0"
    assert _names("Aleva directSTIM Directed", "1")[0] == "K1"
    assert _names("Aleva directSTIM Directed", "2")[0] == "K13"
    assert _levels("Aleva directSTIM Directed", "2")[0] == "K13-15"
    assert _names("NuMed Mini Lead", "2") == ["K0", "K1", "K2", "K3"]
    assert _names("ELAINE Rat Electrode", "1") == ["K0"]
    assert _names("FHC WU Rat Electrode", "2") == ["K0"]


def test_smartflow_keeps_the_gap() -> None:
    assert _names("SmartFlow Cannula NGS-NC-06", "1") == ["K0", "K1"]
    assert _names("SmartFlow Cannula NGS-NC-06", "2") == ["K3", "K4"]


@pytest.mark.parametrize(
    ("model", "left_start"),
    [
        ("SDE-08 S8", 8),
        ("SDE-08 S8 Legacy", 8),
        ("SDE-08 S14", 16),
        ("PMT 2102-16-142", 16),
        ("2069-EPC-05C-35", 8),
        ("DIXI D08-05AM", 5),
        ("DIXI D08-15AM", 15),
        ("AdTech BF09R-SP61X-0BB", 9),
        ("AdTech RD10R-SP07X", 10),
        ("AdTech SD10R-SP05X Choi", 10),
        ("AdTech SD14R-SP05X", 14),
    ],
)
def test_depth_leads_start_at_zero_right_and_per_model_left(
    model: str, left_start: int
) -> None:
    n = get_elspec(model).numContacts
    assert _names(model, "1") == [f"K{i}" for i in range(n)]
    assert _names(model, "2") == [f"K{left_start + i}" for i in range(n)]


def test_unknown_side_and_unknown_model_are_errors() -> None:
    with pytest.raises(ValueError, match="side"):
        contact_names(get_elspec("Medtronic 3389"), "3")  # type: ignore[arg-type]
    fake = dataclasses.replace(get_elspec("Medtronic 3389"), matfname="not_catalogued")
    with pytest.raises(UnspecifiedNaming):
        naming_rule(fake)
