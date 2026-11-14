# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
Emit the resolved electrode geometry that ``dbsloc-reconstruct`` embeds.

The Rust reconstruct path needs the same per-contact model coordinates the Python path builds,
but the derivation lives in :mod:`dbsloc.electrodes.archetypes` and
:mod:`dbsloc.electrodes.layout` (spacing overrides, ``etageidx`` archetypes, segment azimuths,
array span, tail fiducial level, segment arc).
Rather than port that to Rust and let the two drift, the derivation runs once here and the
result is checked in as data. ``tests/test_electrode_geometry_export.py`` regenerates it and
fails on any difference, so a catalogue or layout change that is not re-emitted breaks the
suite instead of silently giving Rust stale geometry.

Usage: ``uv run python scripts/emit_electrode_geometry.py``
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from dbsloc.electrodes.layout import UnsupportedElectrodeLayout, layout_from_spec
from dbsloc.electrodes.names import (
    SIDES,
    contact_names,
    level_names,
    naming_rule,
)
from dbsloc.electrodes.registry import get_elspec, list_models

OUT_PATH = (
    Path(__file__).resolve().parents[1]
    / "crates"
    / "dbsloc-reconstruct"
    / "data"
    / "electrode_geometry.json"
)

_ROUND = 6


def _r(v: float) -> float:
    return round(float(v), _ROUND)


def build_payload() -> dict[str, Any]:
    models: list[dict[str, Any]] = []
    unsupported: list[dict[str, str]] = []
    for name, matfname in list_models():
        spec = get_elspec(name)
        try:
            layout = layout_from_spec(spec)
        except UnsupportedElectrodeLayout as e:
            unsupported.append(
                {"elmodel": name, "matfname": matfname, "reason": str(e)}
            )
            continue
        rule = naming_rule(spec)
        models.append(
            {
                "elmodel": layout.elmodel,
                "matfname": layout.matfname,
                "lead_diameter_mm": _r(layout.lead_diameter_mm),
                "n_contacts": layout.n_contacts,
                "contact_length_mm": _r(layout.contact_length_mm),
                "tip_length_mm": _r(layout.tip_length_mm),
                "tip_is_contact": layout.tip_is_contact,
                "eldist_mm": _r(layout.eldist_mm),
                "level_z_mm": [_r(v) for v in layout.level_z_mm],
                "contact_level": list(layout.contact_level),
                "contact_xyz_mm": [[_r(c) for c in p] for p in layout.contact_xyz_mm],
                "is_directed": layout.is_directed,
                "marker_pos_mm": (
                    None if layout.marker_pos_mm is None else _r(layout.marker_pos_mm)
                ),
                "marker_len_mm": (
                    None if layout.marker_len_mm is None else _r(layout.marker_len_mm)
                ),
                "array_span_mm": _r(layout.array_span_mm),
                "tail_level": layout.tail_level,
                "segment_arc_deg": (
                    None
                    if layout.segment_arc_deg is None
                    else _r(layout.segment_arc_deg)
                ),
                "name_style": rule.style,
                "name_start": {side: rule.start(side) for side in SIDES},
                "contact_names": {
                    side: list(contact_names(spec, side)) for side in SIDES
                },
                "level_names": {side: list(level_names(spec, side)) for side in SIDES},
            }
        )
    return {
        "schema": 3,
        "generator": "scripts/emit_electrode_geometry.py",
        "source": (
            "dbsloc/electrodes/data/catalogue.json via dbsloc.electrodes.layout"
            " and dbsloc.electrodes.names"
        ),
        "unsupported": unsupported,
        "models": models,
    }


def render(payload: dict[str, Any]) -> str:
    return json.dumps(payload, indent=2, ensure_ascii=False) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("-o", "--out", type=Path, default=OUT_PATH)
    ap.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if the file on disk differs from freshly derived geometry",
    )
    args = ap.parse_args()
    text = render(build_payload())
    if args.check:
        have = args.out.read_text(encoding="utf-8") if args.out.is_file() else ""
        if have != text:
            print(f"{args.out} is stale; re-run scripts/emit_electrode_geometry.py")
            return 1
        print(f"{args.out} up to date")
        return 0
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
