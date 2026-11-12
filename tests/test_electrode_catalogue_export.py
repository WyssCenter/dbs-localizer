# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""``dbsloc/electrodes/data/catalogue.json`` matches the ``specs/09`` §6.1 table."""

from __future__ import annotations

from scripts import emit_electrode_catalogue as emit


def test_catalogue_is_not_stale() -> None:
    assert emit.OUT.is_file(), f"{emit.OUT} missing; run the emitter"
    on_disk = emit.OUT.read_text(encoding="utf-8")
    assert on_disk == emit.render(emit.build_rows()), (
        f"{emit.OUT.name} is stale; re-run scripts/emit_electrode_catalogue.py"
    )
