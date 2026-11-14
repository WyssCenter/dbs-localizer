# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

import math

import pytest

from dbsloc.electrodes import ElectrodeSpec, get_elspec, list_models
from dbsloc.electrodes.archetypes import (
    ELDIST_OVERRIDE,
    load_catalogue,
    row_to_spec,
)
from dbsloc.electrodes.layout import layout_from_spec


def test_catalogue_size() -> None:
    assert len(list_models()) == 73


def test_get_elspec() -> None:
    s = get_elspec("Medtronic 3389")
    assert isinstance(s, ElectrodeSpec)
    assert s.matfname == "medtronic_3389"
    assert s.eldist == 2.0
    assert s.contact_span == 7.5
    s2 = get_elspec("medtronic_3389")
    assert s2.elmodel == s.elmodel


def test_3387_3391_span() -> None:
    for name, ex in (("medtronic_3387", 10.5), ("medtronic_3391", 24.0)):
        d = next(x for x in load_catalogue() if x["matfname"] == name)
        s = row_to_spec(d)
        assert abs(s.contact_span - ex) < 1e-9


def test_b33005() -> None:
    d = next(x for x in load_catalogue() if x["matfname"] == "medtronic_b33005")
    s = row_to_spec(d)
    assert s.contact_span == 7.5
    assert s.markerpos is not None
    for idx in s.etageidx:
        if len(idx) == 3:
            break
    else:
        pytest.fail("no triplet in directed etage")
    n = s.numContacts
    assert len(s.contactnames) == 2 * n
    assert sum(len(t) for t in s.etageidx) == n
    assert len(s.etagenames[0]) == len(s.etageidx)


def test_spot_adtech() -> None:
    d1 = next(x for x in load_catalogue() if x["matfname"] == "adtech_bf08r_sp21x")
    cs1 = d1["contact_spacing"]
    assert isinstance(cs1, list) and abs(cs1[0] - 1.43) < 0.01
    d2 = next(x for x in load_catalogue() if x["matfname"] == "adtech_sd06r_sp26x")
    cs2 = d2["contact_spacing"]
    assert isinstance(cs2, list) and abs(float(cs2[2]) - 7.59) < 0.01


def test_nume_elaine() -> None:
    n = get_elspec("NuMed Mini Lead")
    e = get_elspec("ELAINE Rat Electrode")
    f = get_elspec("FHC WU Rat Electrode")
    assert n.contact_span == pytest.approx(3.5)
    assert e.contact_span == pytest.approx(0.1125)
    assert f.contact_span == pytest.approx(0.25)
    assert e.eldist == ELDIST_OVERRIDE["elaine_rat_electrode"] == 0.0


def test_pmt_eldist() -> None:
    for mf, e in (
        ("pmt_2102_16_091", 3.5),
        ("pmt_2102_16_092", 3.97),
        ("pmt_2102_16_142", 5.53),
    ):
        d = next(x for x in load_catalogue() if x["matfname"] == mf)
        s = row_to_spec(d)
        assert abs(s.eldist - e) < 1e-9, mf


def test_bsci_spans() -> None:
    for mf, v in (
        ("boston_vercise_directed", 7.5),
        ("boston_vercise_cartesia_hx", 15.5),
        ("boston_vercise_cartesia_x", 11.5),
    ):
        assert get_elspec(mf).contact_span == pytest.approx(v), mf


def test_tail_fiducial_level_and_fiducial_span() -> None:
    for mf, level, span in (
        ("boston_vercise_cartesia_hx", 3, 6.0),
        ("boston_vercise_cartesia_x", 3, 6.0),
        ("boston_vercise", 3, 6.0),
        ("boston_vercise_directed", 3, 6.0),
        ("medtronic_3389", 3, 6.0),
        ("medtronic_3387", 3, 9.0),
        ("numed_minilead", 3, 3.0),
        ("smartflow_ngs-nc-06", 1, None),
        ("elaine_rat_electrode", 0, 0.0),
    ):
        lay = layout_from_spec(get_elspec(mf))
        assert lay.tail_level == level, mf
        if span is not None:
            assert lay.fiducial_span_mm == pytest.approx(span), mf
    for mf in ("sceneray_sr1202", "sceneray_sr1212"):
        lay = layout_from_spec(get_elspec(mf))
        assert lay.n_levels == 8
        assert lay.tail_level == 7, mf


def test_segment_arc_per_model() -> None:
    for mf, arc in (
        ("boston_vercise_directed", 90.0),
        ("medtronic_b33005", 90.0),
        ("medtronic_b33015", 90.0),
        ("abbott_directed_05", 90.0),
        ("abbott_directed_15", 90.0),
        ("boston_vercise_cartesia_hx", 100.0),
        ("boston_vercise_cartesia_x", 100.0),
        ("aleva_directstim_directed", 85.0),
        ("medtronic_3389", None),
    ):
        assert get_elspec(mf).segment_arc_deg == arc, mf


def _azimuths(mf: str, level: int) -> list[float]:
    lay = layout_from_spec(get_elspec(mf))
    return [
        math.degrees(math.atan2(p[1], p[0])) % 360.0
        for p, lv in zip(lay.contact_xyz_mm, lay.contact_level, strict=True)
        if lv == level
    ]


@pytest.mark.parametrize(
    ("mf", "level", "want"),
    [
        ("abbott_directed_05", 1, [90.0, 330.0, 210.0]),
        ("abbott_directed_15", 2, [90.0, 330.0, 210.0]),
        ("aleva_directstim_directed", 0, [90.0, 330.0, 210.0]),
        ("boston_vercise_directed", 1, [90.0, 210.0, 330.0]),
        ("medtronic_b33005", 2, [90.0, 210.0, 330.0]),
    ],
)
def test_segment_order(mf: str, level: int, want: list[float]) -> None:
    assert _azimuths(mf, level) == pytest.approx(want)


def test_aleva_has_no_marker_length() -> None:
    s = get_elspec("aleva_directstim_directed")
    assert s.markerpos == 13.0
    assert s.markerlen is None
    for mf in ("boston_vercise_directed", "medtronic_b33005", "abbott_directed_05"):
        assert get_elspec(mf).markerlen is not None, mf


def test_directed_model_without_marker_length_is_rejected() -> None:
    row = dict(
        next(x for x in load_catalogue() if x["matfname"] == "abbott_directed_05")
    )
    row["markerlen"] = None
    with pytest.raises(ValueError, match="markerlen"):
        row_to_spec(row)
