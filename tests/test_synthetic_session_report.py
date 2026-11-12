# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""The committed synthetic session reports equal what ``scripts/make_synthetic_session_report.py`` writes."""

from __future__ import annotations

import json
from pathlib import Path

from scripts import make_synthetic_session_report as gen

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "reports" / "synthetic"


def test_generator_reproduces_the_committed_files(tmp_path: Path) -> None:
    written = gen.write_all(tmp_path)
    assert {p.name for p in written} == {p.name for p in FIXTURES.iterdir()}
    for path in written:
        committed = (FIXTURES / path.name).read_bytes()
        if path.suffix == ".json":
            assert path.read_bytes() == committed.replace(b"\r\n", b"\n"), path.name
        else:
            assert path.read_bytes() == committed, path.name


def test_generation_is_deterministic(tmp_path: Path) -> None:
    first = gen.write_all(tmp_path / "a")
    second = gen.write_all(tmp_path / "b")
    for a, b in zip(first, second, strict=True):
        assert a.read_bytes() == b.read_bytes(), a.name


def test_reports_carry_no_vendor_artwork_fonts_or_metadata() -> None:
    for rep in gen.reports():
        pdf, meta = gen.render(rep)
        assert pdf.startswith(b"%PDF-1.4\n")
        pdf.decode("ascii")
        for forbidden in (
            b"/Producer",
            b"/Creator",
            b"/Info",
            b"/Metadata",
            b"/FontFile",
            b"/CreationDate",
            b"Medtronic",
            b"Roboto",
            b"DEMO MODE",
        ):
            assert forbidden not in pdf, (rep.slug, forbidden)
        assert b"Session Report" in pdf
        parsed = json.loads(meta)
        assert parsed["file"] == f"{rep.slug}.pdf"
        assert parsed["pages"] == len(gen.build_pages(rep))


def test_scenario_covers_both_lead_families_and_both_devices() -> None:
    ring, directional = gen.reports()
    assert (ring.kind, ring.directional, ring.recharge is None) == ("PC", False, True)
    assert (
        directional.kind,
        directional.directional,
        directional.recharge is None,
    ) == (
        "RC",
        True,
        False,
    )
    assert ring.survey and not directional.survey
    assert ring.identifier is not None and directional.identifier is None
    assert directional.eri is not None and ring.battery_estimate is not None
