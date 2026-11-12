# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

import pytest

from dbsloc.electrodes import ElectrodeSpec, get_elspec, list_models
from dbsloc.electrodes.archetypes import (
    ELDIST_OVERRIDE,
    SPAN_OVERRIDE,
    load_catalogue,
    row_to_spec,
)


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
    assert n.contact_span == SPAN_OVERRIDE["numed_minilead"] == 2.0
    assert e.contact_span == SPAN_OVERRIDE["elaine_rat_electrode"] == 0.1125
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
        ("boston_vercise_directed", 9.0),
        ("boston_vercise_cartesia_hx", 16.5),
        ("boston_vercise_cartesia_x", 11.5),
    ):
        assert get_elspec(mf).contact_span == v
