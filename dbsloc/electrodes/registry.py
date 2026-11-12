# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""`get_elspec` / `list_models` — `specs/09` §4.4."""

from __future__ import annotations

from dbsloc.electrodes.archetypes import load_catalogue, row_to_spec
from dbsloc.electrodes.spec import ElectrodeSpec

_by_display: dict[str, ElectrodeSpec] = {}
_by_matfname: dict[str, ElectrodeSpec] = {}


def _ensure() -> None:
    if _by_matfname:
        return
    for row in load_catalogue():
        spec = row_to_spec(row)
        _by_display[spec.elmodel] = spec
        _by_matfname[spec.matfname] = spec


def get_elspec(model_name: str) -> ElectrodeSpec:
    """Resolve by display name or ``matfname``."""
    _ensure()
    if model_name in _by_display:
        return _by_display[model_name]
    if model_name in _by_matfname:
        return _by_matfname[model_name]
    raise KeyError(model_name)


def list_models() -> list[tuple[str, str]]:
    """(``elmodel``, ``matfname``) for all 73 models."""
    _ensure()
    return sorted((s.elmodel, s.matfname) for s in _by_matfname.values())


__all__ = ["get_elspec", "list_models"]
