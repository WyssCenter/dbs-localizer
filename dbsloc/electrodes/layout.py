# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Model-frame contact layout for any catalogue row (``specs/09`` §2.2, §3.1–§3.2)."""

from __future__ import annotations

import math
from dataclasses import dataclass
from typing import Final

from dbsloc.electrodes.spec import ElectrodeSpec

__all__ = (
    "ElectrodeLayout",
    "UnsupportedElectrodeLayout",
    "layout_from_spec",
)

_CLOCKWISE_SEGMENTS: Final[frozenset[str]] = frozenset(
    {"abbott_directed_05", "abbott_directed_15", "aleva_directstim_directed"}
)

_FIRST_SEGMENT_DEG: Final[float] = 90.0


class UnsupportedElectrodeLayout(ValueError):
    """The catalogue row describes a level arrangement with no geometric convention."""


@dataclass(frozen=True, slots=True)
class ElectrodeLayout:
    """
    Resolved geometry of one lead in its local **RAS-like** model frame.

    Origin is the centre of the most-distal *level* (not the tip apex, which ``specs/09`` §3.1
    uses), so ``level_z_mm[0] == 0`` and the origin coincides with the ``head`` fiducial that
    reconstruction solves for. ``+z`` runs proximally along the shaft.
    """

    elmodel: str
    matfname: str
    lead_diameter_mm: float
    n_contacts: int
    contact_length_mm: float
    tip_length_mm: float
    tip_is_contact: bool
    eldist_mm: float
    level_z_mm: tuple[float, ...]
    contact_level: tuple[int, ...]
    contact_xyz_mm: tuple[tuple[float, float, float], ...]
    is_directed: bool
    marker_pos_mm: float | None
    marker_len_mm: float | None
    array_span_mm: float
    tail_level: int
    segment_arc_deg: float | None

    @property
    def n_levels(self) -> int:
        return len(self.level_z_mm)

    @property
    def level_pitches_mm(self) -> tuple[float, ...]:
        z = self.level_z_mm
        return tuple(z[i] - z[i - 1] for i in range(1, len(z)))

    @property
    def fiducial_span_mm(self) -> float:
        """Head to tail fiducial (``specs/09`` §2.2), the expected ``head→tail`` distance."""
        return self.level_z_mm[self.tail_level] - self.level_z_mm[0]


def _level_pitches(spec: ElectrodeSpec, n_levels: int) -> tuple[float, ...]:
    """
    Centre-to-centre pitch per level gap.

    ``eldist`` is a single nominal pitch taken from the *second* spacing value (``specs/09``
    §2.1), so it is wrong for the first gap of the non-uniform SEEG/depth leads. Those rows
    carry one gap per level, so the per-gap pitch is used directly where it is available.
    """
    cl = float(spec.contact_length)
    cs = spec.contact_spacing
    n_gaps = max(0, n_levels - 1)
    if isinstance(cs, list) and len(cs) == n_gaps:
        return tuple(float(g) + cl for g in cs)
    return tuple(float(spec.eldist) for _ in range(n_gaps))


def _level_offsets(
    n_in_level: int, radius_mm: float, clockwise: bool, *, elmodel: str, level: int
) -> tuple[tuple[float, float], ...]:
    """
    In-plane ``(x, y)`` offset of each contact within one level.

    A ring sits on the axis; segments sit on the shaft surface. ``specs/09`` §3.2 puts segment
    A at ``+Y`` (hence :data:`_FIRST_SEGMENT_DEG`) with the rest at equal azimuth steps,
    counter-clockwise seen from the proximal end — except Abbott Directed and Aleva
    directSTIM (:data:`_CLOCKWISE_SEGMENTS`, ``specs/09`` §2.2), which are clockwise. Getting
    that handedness backwards mirrors the segment labels: span and pitch stay correct, so
    nothing downstream catches it.
    """
    if n_in_level == 1:
        return ((0.0, 0.0),)
    if n_in_level != 3:
        raise UnsupportedElectrodeLayout(
            f"{elmodel!r}: level {level} has {n_in_level} contacts; specs/09 §3.2 defines "
            "radial placement for 1 (ring) and 3 (segmented) only"
        )
    step = -120.0 if clockwise else 120.0
    return tuple(
        (
            radius_mm * math.cos(math.radians(_FIRST_SEGMENT_DEG + step * i)),
            radius_mm * math.sin(math.radians(_FIRST_SEGMENT_DEG + step * i)),
        )
        for i in range(n_in_level)
    )


def layout_from_spec(spec: ElectrodeSpec) -> ElectrodeLayout:
    """Resolve a catalogue row to per-contact model coordinates and level membership."""
    eix = tuple(tuple(int(i) for i in lv) for lv in spec.etageidx)
    n = int(spec.numContacts)
    if not eix:
        raise UnsupportedElectrodeLayout(f"{spec.elmodel!r}: etageidx is empty")
    flat = sorted(i for lv in eix for i in lv)
    if flat != list(range(1, n + 1)):
        raise UnsupportedElectrodeLayout(
            f"{spec.elmodel!r}: etageidx covers {flat}, expected 1..{n}"
        )

    pitches = _level_pitches(spec, len(eix))
    z: list[float] = [0.0]
    for p in pitches:
        z.append(z[-1] + p)

    radius = 0.5 * float(spec.lead_diameter)
    clockwise = spec.matfname in _CLOCKWISE_SEGMENTS
    xyz: list[tuple[float, float, float]] = [(0.0, 0.0, 0.0)] * n
    level_of: list[int] = [0] * n
    for k, lv in enumerate(eix):
        offsets = _level_offsets(
            len(lv), radius, clockwise, elmodel=spec.elmodel, level=k
        )
        for contact_1based, (dx, dy) in zip(lv, offsets, strict=True):
            xyz[contact_1based - 1] = (dx, dy, z[k])
            level_of[contact_1based - 1] = k

    return ElectrodeLayout(
        elmodel=spec.elmodel,
        matfname=spec.matfname,
        lead_diameter_mm=float(spec.lead_diameter),
        n_contacts=n,
        contact_length_mm=float(spec.contact_length),
        tip_length_mm=float(spec.tip_length),
        tip_is_contact=bool(spec.tipiscontact),
        eldist_mm=float(spec.eldist),
        level_z_mm=tuple(z),
        contact_level=tuple(level_of),
        contact_xyz_mm=tuple(xyz),
        is_directed=bool(spec.isdirected),
        marker_pos_mm=None if spec.markerpos is None else float(spec.markerpos),
        marker_len_mm=None if spec.markerlen is None else float(spec.markerlen),
        array_span_mm=float(spec.contact_span),
        tail_level=int(spec.tail_level),
        segment_arc_deg=(
            None if spec.segment_arc_deg is None else float(spec.segment_arc_deg)
        ),
    )
