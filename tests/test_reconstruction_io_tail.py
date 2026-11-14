# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

import copy
import json
import logging
import math

import pytest

from dbsloc.electrodes import get_elspec
from dbsloc.electrodes.layout import layout_from_spec
from dbsloc.reconstruction.io import (
    convert_legacy_tail,
    read_recon_json_v2,
    tail_conversion_warning,
    trajectory,
    write_recon_json_v2,
)

HX = "Boston Scientific Vercise Cartesia HX"
VD = "Boston Scientific Vercise Directed"
LOGGER = "dbsloc.reconstruction.io"


def _side(head: list[float], span: float, model: str) -> dict:
    layout = layout_from_spec(get_elspec(model))
    tail = [head[0], head[1], head[2] + span]
    return {
        "coords_mm": [[1.0, 2.0, 3.0]],
        "trajectory": trajectory(head, tail, layout),
        "markers": {
            "head": head,
            "tail": tail,
            "x": [head[0] + 0.65, head[1], head[2]],
            "y": [head[0], head[1] + 0.65, head[2]],
        },
    }


def _file(model: str, native_span: float | None, mni_span: float) -> dict:
    d = {
        "version": 2,
        "subj_id": "1",
        "method": "m",
        "props": {"1": {"elmodel": model}},
        "meta": {"elmodel": model},
        "mni": {"1": _side([12.0, -12.0, -8.0], mni_span, model)},
    }
    if native_span is not None:
        d["native"] = {"1": _side([10.0, -5.0, -3.0], native_span, model)}
    return d


def _write(tmp_path, d: dict):
    p = tmp_path / "sub-1_desc-reconstruction.json"
    p.write_text(json.dumps(d), encoding="utf-8")
    return p


def _messages(caplog) -> list[str]:
    return [r.getMessage() for r in caplog.records if r.name == LOGGER]


def test_old_convention_hx_file_is_converted_on_read(tmp_path, caplog) -> None:
    p = _write(tmp_path, _file(HX, 14.0, 15.4))
    with caplog.at_level(logging.WARNING, logger=LOGGER):
        r = read_recon_json_v2(p)
    assert _messages(caplog) == [tail_conversion_warning("1")]
    assert tail_conversion_warning("1") == (
        "dbsloc: warning: reconstruction: side 1: tail marker moved from the last "
        "to the 4th contact level (file written before 2026-11-14)"
    )
    assert r.native is not None
    assert r.native["1"]["markers"]["tail"] == pytest.approx([10.0, -5.0, 3.0])
    assert r.mni["1"]["markers"]["tail"] == pytest.approx(
        [12.0, -12.0, -8.0 + 15.4 * 6.0 / 14.0]
    )
    assert r.native["1"]["markers"]["head"] == [10.0, -5.0, -3.0]
    assert r.native["1"]["coords_mm"] == [[1.0, 2.0, 3.0]]
    end = r.native["1"]["trajectory"][-1]
    assert end == pytest.approx([10.0, -5.0, 3.0 + 2.0 * 15.5])

    out = tmp_path / "rewritten.json"
    write_recon_json_v2(out, r)
    again = json.loads(out.read_text(encoding="utf-8"))
    assert again["meta"]["tail_fiducial"] == "fourth_level"
    assert again["native"]["1"]["markers"]["tail"] == pytest.approx([10.0, -5.0, 3.0])


def test_every_read_logs_its_conversion(tmp_path, caplog) -> None:
    p = _write(tmp_path, _file(HX, 14.0, 14.0))
    with caplog.at_level(logging.WARNING, logger=LOGGER):
        read_recon_json_v2(p)
        read_recon_json_v2(p)
    assert _messages(caplog) == [tail_conversion_warning("1")] * 2


def test_template_only_old_file_decides_from_the_mni_block(tmp_path, caplog) -> None:
    p = _write(tmp_path, _file(HX, None, 14.0))
    with caplog.at_level(logging.WARNING, logger=LOGGER):
        r = read_recon_json_v2(p)
    assert _messages(caplog) == [tail_conversion_warning("1")]
    assert r.mni["1"]["markers"]["tail"] == pytest.approx([12.0, -12.0, -2.0])


def test_a_side_only_in_mni_decides_from_the_mni_block() -> None:
    d = _file(HX, 6.0, 14.0)
    d["mni"]["2"] = _side([-12.0, -12.0, -8.0], 14.0, HX)
    d["props"]["2"] = {"elmodel": HX}
    assert convert_legacy_tail(d) == [tail_conversion_warning("2")]
    assert d["mni"]["2"]["markers"]["tail"] == pytest.approx([-12.0, -12.0, -2.0])
    assert d["mni"]["1"]["markers"]["tail"] == pytest.approx([12.0, -12.0, 6.0])


@pytest.mark.parametrize(
    ("model", "native_span", "mni_span"),
    [(HX, 6.1, 6.6), (VD, 6.0, 6.0)],
)
def test_reference_style_and_four_level_files_are_unchanged(
    tmp_path, caplog, model: str, native_span: float, mni_span: float
) -> None:
    d = _file(model, native_span, mni_span)
    with caplog.at_level(logging.WARNING, logger=LOGGER):
        r = read_recon_json_v2(_write(tmp_path, d))
    assert _messages(caplog) == []
    assert r.native == d["native"]
    assert r.mni == d["mni"]


def test_a_file_with_the_field_is_not_converted(tmp_path, caplog) -> None:
    d = _file(HX, 14.0, 14.0)
    d["meta"]["tail_fiducial"] = "fourth_level"
    with caplog.at_level(logging.WARNING, logger=LOGGER):
        r = read_recon_json_v2(_write(tmp_path, d))
    assert _messages(caplog) == []
    assert r.native is not None
    assert math.isclose(r.native["1"]["markers"]["tail"][2], 11.0)


def test_model_lookup_follows_the_catalogue_rules() -> None:
    for name in [
        "  boston scientific vercise cartesia hx ",
        "BOSTON_VERCISE_CARTESIA_HX",
    ]:
        d = _file(HX, 14.0, 14.0)
        d["props"]["1"]["elmodel"] = name
        d["meta"].pop("elmodel")
        assert convert_legacy_tail(d) == [tail_conversion_warning("1")], name

    d = _file(HX, 14.0, 14.0)
    d["props"]["1"]["elmodel"] = 42
    assert convert_legacy_tail(d) == [tail_conversion_warning("1")]

    d = _file(HX, 14.0, 14.0)
    d["props"]["1"]["elmodel"] = ""
    assert convert_legacy_tail(d) == []

    d = _file(HX, 14.0, 14.0)
    d.pop("props")
    d.pop("meta")
    assert convert_legacy_tail(copy.deepcopy(d)) == []
    assert convert_legacy_tail(d, HX) == [tail_conversion_warning("1")]


def test_malformed_parts_skip_the_side_instead_of_raising() -> None:
    d = _file(HX, 14.0, 14.0)
    d["props"] = ["not", "a", "map"]
    d["meta"] = "not a map"
    assert convert_legacy_tail(d) == []
    assert convert_legacy_tail(d, HX) == [tail_conversion_warning("1")]

    d = _file(HX, 14.0, 14.0)
    d["props"]["1"] = "not a map"
    assert convert_legacy_tail(d) == [tail_conversion_warning("1")]

    d = _file(HX, 14.0, 14.0)
    d["reference"] = "not a map"
    d["scrf"] = {"1": "not a map"}
    assert convert_legacy_tail(d) == [tail_conversion_warning("1")]

    d = _file(HX, 14.0, 14.0)
    d["native"]["1"]["markers"]["tail"] = [10.0, -5.0, True]
    d["mni"]["1"]["markers"]["tail"] = [12.0, -12.0, "6"]
    before = copy.deepcopy(d)
    assert convert_legacy_tail(d) == []
    assert d == before
