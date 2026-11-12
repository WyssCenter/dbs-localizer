# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""`ElectrodeSpec` — `specs/09_electrode_models.md` §5."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class ElectrodeSpec:
    elmodel: str
    matfname: str
    lead_diameter: float
    tip_diameter: float
    tip_length: float
    tipiscontact: bool
    numContacts: int
    contact_length: float
    contact_diameter: float
    contact_spacing: float | list[float]
    eldist: float
    contact_span: float
    isdirected: bool
    forstimulation: bool
    isdio_supported: bool
    contactnames: tuple[str, ...]
    etagenames: tuple[tuple[str, ...], tuple[str, ...]]
    etageidx: tuple[tuple[int, ...], ...]
    markerpos: float | None
    markerlen: float | None


__all__ = ["ElectrodeSpec"]
