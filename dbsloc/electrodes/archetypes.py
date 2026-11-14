# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Geometry + etage from catalogue rows (`specs/09` §2.1, §2.2, §5)."""

from __future__ import annotations

import json
import typing
from pathlib import Path
from typing import Any, Final

from dbsloc.electrodes.spec import ElectrodeSpec

ELDIST_OVERRIDE: Final[dict[str, float]] = {
    "boston_vercise_directed": 2.0,
    "boston_vercise_cartesia_hx": 2.0,
    "boston_vercise_cartesia_x": 2.0,
    "pmt_2102_16_091": 3.5,
    "pmt_2102_16_092": 3.97,
    "pmt_2102_16_142": 5.53,
    "neuropace_dl_344_10": 10.0,
    "elaine_rat_electrode": 0.0,
    "fhc_wu_rat_electrode": 0.0,
    "numed_minilead": 1.0,
}
DEFAULT_SEGMENT_ARC_DEG: Final[float] = 90.0
SEGMENT_ARC_DEG: Final[dict[str, float]] = {
    "boston_vercise_cartesia_hx": 100.0,
    "boston_vercise_cartesia_x": 100.0,
    "aleva_directstim_directed": 85.0,
}
TAIL_FIDUCIAL_LEVEL: Final[int] = 3
TAIL_AT_LAST_LEVEL: Final[frozenset[str]] = frozenset(
    {"sceneray_sr1202", "sceneray_sr1212"}
)
MARKERLEN_UNDEFINED: Final[frozenset[str]] = frozenset({"aleva_directstim_directed"})


def _ring_pair_names(n: int) -> tuple[str, ...]:
    return tuple(f"R{i + 1}" for i in range(n)) + tuple(f"L{i + 1}" for i in range(n))


def build_etage(
    arch: str, n: int
) -> tuple[tuple[str, ...], tuple[tuple[str, ...], tuple[str, ...]], tuple]:
    if arch == "ring1":
        return ("C1R", "C1L"), (("C1R",), ("C1L",)), ((1,),)
    if arch in ("ring4", "ring4_tip"):
        eix = tuple((i + 1,) for i in range(4))
        eg = (
            tuple(f"K{i + 1}R" for i in range(4)),
            tuple(f"K{i + 1}L" for i in range(4)),
        )
        return _ring_pair_names(4), eg, eix
    if arch == "ring8":
        eix = tuple((i + 1,) for i in range(8))
        eg = (
            tuple(f"K{i + 1}R" for i in range(8)),
            tuple(f"K{i + 1}L" for i in range(8)),
        )
        return _ring_pair_names(8), eg, eix
    if arch == "m8d":
        eix = ((1,), (2, 3, 4), (5, 6, 7), (8,))
        eg = (("K0R", "K1R", "K2R", "K3R"), ("K0L", "K1L", "K2L", "K3L"))
        cn = tuple(f"K{i}R" for i in range(8)) + tuple(f"K{i}L" for i in range(8))
        return cn, eg, eix
    if arch in ("b8d_tip", "a8d"):
        eix = ((1,), (2, 3, 4), (5, 6, 7), (8,))
        if arch == "b8d_tip":
            eg = (("G0R", "G1R", "G2R", "G3R"), ("G0L", "G1L", "G2L", "G3L"))
        else:
            eg = (("A0R", "A1R", "A2R", "A3R"), ("A0L", "A1L", "A2L", "A3L"))
        cn = tuple(f"K{i + 1}R" for i in range(8)) + tuple(
            f"K{i + 1}L" for i in range(8)
        )
        return cn, eg, eix
    if arch == "a12d":
        eix = ((1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12))
        eg = (("AR1", "AR2", "AR3", "AR4"), ("AL1", "AL2", "AL3", "AL4"))
        cn = tuple(f"K{i + 1}R" for i in range(12)) + tuple(
            f"K{i + 1}L" for i in range(12)
        )
        return cn, eg, eix
    if arch == "c16_hx":
        eix = (
            (1, 2, 3),
            (4, 5, 6),
            (7, 8, 9),
            (10, 11, 12),
            (13,),
            (14,),
            (15,),
            (16,),
        )
        eg = (
            tuple(f"HR{i + 1}" for i in range(8)),
            tuple(f"HL{i + 1}" for i in range(8)),
        )
        cn = tuple(f"K{i + 1}R" for i in range(16)) + tuple(
            f"K{i + 1}L" for i in range(16)
        )
        return cn, eg, eix
    if arch == "c16_x":
        eix = ((1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12), (13, 14, 15), (16,))
        eg = (
            tuple(f"XR{i + 1}" for i in range(6)),
            tuple(f"XL{i + 1}" for i in range(6)),
        )
        cn = tuple(f"K{i + 1}R" for i in range(16)) + tuple(
            f"K{i + 1}L" for i in range(16)
        )
        return cn, eg, eix
    if arch == "smart2":
        return (
            ("C0R", "C1R", "C0L", "C1L"),
            (("C0R", "C1R"), ("C0L", "C1L")),
            ((1,), (2,)),
        )
    if arch.startswith("seeg_"):
        nn = n
        eix = tuple((i + 1,) for i in range(nn))
        eg = (
            tuple(f"E{i + 1}R" for i in range(nn)),
            tuple(f"E{i + 1}L" for i in range(nn)),
        )
        return _ring_pair_names(nn), eg, eix
    raise KeyError(arch)


def _as_cs(row: object) -> float | list[float]:
    if isinstance(row, list):
        lst = typing.cast(list[Any], row)
        return [float(x) for x in lst]
    return float(typing.cast(Any, row))


def _eldist(cl: float, cs: float | list[float]) -> float:
    if isinstance(cs, list) and len(cs) > 1:
        return float(cs[1]) + cl
    if isinstance(cs, list) and len(cs) == 1:
        return float(cs[0]) + cl
    if isinstance(cs, list) and len(cs) == 0:
        return cl
    return float(typing.cast(float, cs)) + cl


def _span(
    tip_len: float,
    tipis: bool,
    cl: float,
    cs: float | list[float],
    n_lev: int,
) -> float:
    t = 1 if tipis else 0
    if isinstance(cs, list) and len(cs) > 1:
        s = float(sum(cs))  # type: ignore[arg-type]
        return float(tip_len * t + cl * (n_lev - t) + s)
    c = float(cs) if not isinstance(cs, list) else float(cs[0])
    return float(tip_len * t + cl * (n_lev - t) + c * (n_lev - 1))


def tail_level(matfname: str, n_levels: int) -> int:
    if matfname in TAIL_AT_LAST_LEVEL or n_levels <= TAIL_FIDUCIAL_LEVEL:
        return max(0, n_levels - 1)
    return TAIL_FIDUCIAL_LEVEL


def segment_arc_deg(matfname: str, isdirected: bool) -> float | None:
    if not isdirected:
        return None
    return SEGMENT_ARC_DEG.get(matfname, DEFAULT_SEGMENT_ARC_DEG)


def row_to_spec(d: dict[str, Any]) -> ElectrodeSpec:
    arch = str(d["archetype"])
    n = int(d["numContacts"])
    cl = float(d["contact_length"])
    cs: float | list[float] = _as_cs(d["contact_spacing"])  # type: ignore[arg-type]
    tipis = bool(d["tipiscontact"])
    cn, eg, eix = build_etage(arch, n)
    n_lev = len(eix)
    eid = _eldist(cl, cs)
    sp = _span(float(d["tip_length"]), tipis, cl, cs, n_lev)
    mfn = str(d["matfname"])
    if mfn in ELDIST_OVERRIDE:
        eid = ELDIST_OVERRIDE[mfn]
    mp, ml = d.get("markerpos"), d.get("markerlen")
    mpos = None if mp is None else float(mp)
    mlen = None if ml is None else float(ml)
    directed = bool(d["isdirected"])
    if directed and mpos is None:
        raise ValueError(f"directed model missing markerpos: {mfn!r}")
    if directed and mlen is None and mfn not in MARKERLEN_UNDEFINED:
        raise ValueError(f"directed model missing markerlen: {mfn!r}")
    return ElectrodeSpec(
        elmodel=str(d["elmodel"]),
        matfname=mfn,
        lead_diameter=float(d["lead_diameter"]),
        tip_diameter=float(d["tip_diameter"]),
        contact_length=cl,
        contact_diameter=float(d["contact_diameter"]),
        tip_length=float(d["tip_length"]),
        tipiscontact=tipis,
        numContacts=n,
        contact_spacing=cs,
        eldist=eid,
        contact_span=sp,
        isdirected=directed,
        forstimulation=bool(d.get("forstimulation", True)),
        isdio_supported=bool(d.get("isdio_supported", False)),
        contactnames=cn,
        etagenames=eg,
        etageidx=eix,  # type: ignore[type-arg]
        markerpos=mpos,
        markerlen=mlen,
        tail_level=tail_level(mfn, n_lev),
        segment_arc_deg=segment_arc_deg(mfn, directed),
    )


def load_catalogue() -> list[dict[str, object]]:
    path = Path(__file__).resolve().parent / "data" / "catalogue.json"
    with path.open(encoding="utf-8") as f:
        return json.load(f)  # type: ignore[no-any-return]


__all__ = [
    "DEFAULT_SEGMENT_ARC_DEG",
    "ELDIST_OVERRIDE",
    "MARKERLEN_UNDEFINED",
    "SEGMENT_ARC_DEG",
    "TAIL_AT_LAST_LEVEL",
    "TAIL_FIDUCIAL_LEVEL",
    "_as_cs",
    "_eldist",
    "_span",
    "build_etage",
    "load_catalogue",
    "row_to_spec",
    "segment_arc_deg",
    "tail_level",
]
