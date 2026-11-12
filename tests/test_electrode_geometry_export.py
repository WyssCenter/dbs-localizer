# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""``crates/dbsloc-reconstruct/data/electrode_geometry.json`` matches the live derivation."""

from __future__ import annotations

import json

import pytest

from dbsloc.electrodes.layout import layout_from_spec
from dbsloc.electrodes.registry import get_elspec
from scripts import emit_electrode_geometry as emit


def test_embedded_geometry_is_not_stale() -> None:
    """
    The Rust reconstruct path embeds this file at compile time, so a catalogue or layout change
    that is not re-emitted would leave Rust computing contacts from stale geometry with nothing
    to signal it. Run ``uv run python scripts/emit_electrode_geometry.py`` to refresh.
    """
    assert emit.OUT_PATH.is_file(), f"{emit.OUT_PATH} missing; run the emitter"
    on_disk = emit.OUT_PATH.read_text(encoding="utf-8")
    assert on_disk == emit.render(emit.build_payload()), (
        f"{emit.OUT_PATH.name} is stale; re-run scripts/emit_electrode_geometry.py"
    )


def test_exported_geometry_agrees_with_the_python_model() -> None:
    """Field-by-field, so a silent shape or unit change in the emitter cannot slip through."""
    payload = json.loads(emit.OUT_PATH.read_text(encoding="utf-8"))
    assert payload["unsupported"] == []
    assert payload["models"], "no models exported"
    for row in payload["models"]:
        layout = layout_from_spec(get_elspec(row["elmodel"]))
        assert row["n_contacts"] == layout.n_contacts, row["elmodel"]
        assert row["contact_level"] == list(layout.contact_level), row["elmodel"]
        assert row["is_directed"] == layout.is_directed, row["elmodel"]
        assert row["lead_diameter_mm"] == pytest.approx(layout.lead_diameter_mm), row[
            "elmodel"
        ]
        assert row["eldist_mm"] == pytest.approx(layout.eldist_mm), row["elmodel"]
        assert row["level_z_mm"] == pytest.approx(list(layout.level_z_mm)), row[
            "elmodel"
        ]
        assert len(row["contact_xyz_mm"]) == layout.n_contacts, row["elmodel"]
        for got, want in zip(row["contact_xyz_mm"], layout.contact_xyz_mm, strict=True):
            assert got == pytest.approx(list(want)), row["elmodel"]
