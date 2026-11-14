# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

from __future__ import annotations

from dataclasses import dataclass
from typing import Final, Literal

from dbsloc.electrodes.spec import ElectrodeSpec

__all__ = (
    "NAMING_RULES",
    "SIDES",
    "NamingRule",
    "UnspecifiedNaming",
    "contact_names",
    "level_names",
    "naming_rule",
)

type NameStyle = Literal["numeric", "level_letter"]
type Side = Literal["1", "2"]

SIDES: Final[tuple[Side, Side]] = ("1", "2")

_SEGMENT_LETTERS: Final[str] = "ABC"


class UnspecifiedNaming(KeyError):
    pass


@dataclass(frozen=True, slots=True)
class NamingRule:
    right_start: int
    left_start: int
    style: NameStyle

    def start(self, side: Side) -> int:
        if side == "1":
            return self.right_start
        if side == "2":
            return self.left_start
        raise ValueError(f"side must be '1' (right) or '2' (left), not {side!r}")


def _numeric(right: int, left: int) -> NamingRule:
    return NamingRule(right, left, "numeric")


def _level_letter(right: int, left: int) -> NamingRule:
    return NamingRule(right, left, "level_letter")


_DEPTH_LEFT_START: Final[dict[str, int]] = {
    "sde_08_s8": 8,
    "sde_08_s8_legacy": 8,
    "sde_08_s10": 10,
    "sde_08_s10_legacy": 10,
    "sde_08_s12": 12,
    "sde_08_s12_legacy": 12,
    "sde_08_s14": 16,
    "sde_08_s16": 16,
    "sde_08_s16_legacy": 16,
    "pmt_2102_04_091": 4,
    "pmt_2102_06_091": 6,
    "pmt_2102_08_091": 8,
    "pmt_2102_10_091": 10,
    "pmt_2102_12_091": 12,
    "pmt_2102_14_091": 14,
    "pmt_2102_16_091": 16,
    "pmt_2102_16_092": 16,
    "pmt_2102_16_093": 16,
    "pmt_2102_16_131": 16,
    "pmt_2102_16_142": 16,
    "epc_05c": 8,
    "epc_15c": 16,
    "dixi_d08_05am": 5,
    "dixi_d08_08am": 8,
    "dixi_d08_10am": 10,
    "dixi_d08_12am": 12,
    "dixi_d08_15am": 15,
    "dixi_d08_18am": 18,
    "adtech_bf08r_sp05x": 8,
    "adtech_bf08r_sp21x": 8,
    "adtech_bf08r_sp61x": 8,
    "adtech_bf09r_sp61x_0bb": 9,
    "adtech_rd06r_sp05x": 6,
    "adtech_rd08r_sp05x": 8,
    "adtech_rd10r_sp03x": 10,
    "adtech_rd10r_sp05x": 10,
    "adtech_rd10r_sp06x": 10,
    "adtech_rd10r_sp07x": 10,
    "adtech_rd10r_sp08x": 10,
    "adtech_sd06r_sp26x": 6,
    "adtech_sd08r_sp05x": 8,
    "adtech_sd10r_sp05x": 10,
    "adtech_sd10r_sp05x_choi": 10,
    "adtech_sd14r_sp05x": 14,
}

NAMING_RULES: Final[dict[str, NamingRule]] = {
    **dict.fromkeys(
        (
            "medtronic_3387",
            "medtronic_3389",
            "medtronic_3391",
            "pins_l301",
            "pins_l302",
            "pins_l303",
            "neuropace_dl_344_10",
            "neuropace_dl_344_35",
        ),
        _numeric(0, 8),
    ),
    **dict.fromkeys(("medtronic_b33005", "medtronic_b33015"), _level_letter(0, 0)),
    **dict.fromkeys(("boston_vercise", "boston_vercise_directed"), _numeric(9, 1)),
    **dict.fromkeys(
        ("boston_vercise_cartesia_hx", "boston_vercise_cartesia_x"), _numeric(17, 1)
    ),
    **dict.fromkeys(("abbott_activetip_2mm", "abbott_activetip_3mm"), _numeric(1, 1)),
    **dict.fromkeys(("abbott_directed_05", "abbott_directed_15"), _level_letter(1, 1)),
    **dict.fromkeys(
        ("sceneray_sr1200", "sceneray_sr1210", "sceneray_sr1211", "sceneray_sr1242"),
        _numeric(4, 0),
    ),
    **dict.fromkeys(("sceneray_sr1202", "sceneray_sr1212"), _numeric(8, 0)),
    "aleva_directstim_directed": _numeric(1, 13),
    **dict.fromkeys(
        ("numed_minilead", "elaine_rat_electrode", "fhc_wu_rat_electrode"),
        _numeric(0, 0),
    ),
    "smartflow_ngs-nc-06": _numeric(0, 3),
    **{m: _numeric(0, left) for m, left in _DEPTH_LEFT_START.items()},
}


def naming_rule(spec: ElectrodeSpec) -> NamingRule:
    try:
        return NAMING_RULES[spec.matfname]
    except KeyError:
        raise UnspecifiedNaming(
            f"{spec.elmodel!r} ({spec.matfname}) has no contact naming rule in specs/09 §2.3"
        ) from None


def _contact_levels(spec: ElectrodeSpec) -> list[tuple[int, int, int]]:
    out: list[tuple[int, int, int]] = []
    for level, members in enumerate(spec.etageidx):
        for pos, contact_1based in enumerate(members):
            out.append((int(contact_1based) - 1, level, pos))
    out.sort()
    return out


def _segment_letter(spec: ElectrodeSpec, level: int, pos: int) -> str:
    n = len(spec.etageidx[level])
    if n == 1:
        return ""
    if n != len(_SEGMENT_LETTERS):
        raise UnspecifiedNaming(
            f"{spec.elmodel!r}: level {level} has {n} contacts; specs/09 §2.3 names "
            "segments A / B / C only"
        )
    return _SEGMENT_LETTERS[pos]


def contact_names(spec: ElectrodeSpec, side: Side) -> tuple[str, ...]:
    rule = naming_rule(spec)
    start = rule.start(side)
    if rule.style == "numeric":
        return tuple(f"K{start + i}" for i in range(int(spec.numContacts)))
    return tuple(
        f"K{start + level}{_segment_letter(spec, level, pos)}"
        for _, level, pos in _contact_levels(spec)
    )


def level_names(spec: ElectrodeSpec, side: Side) -> tuple[str, ...]:
    rule = naming_rule(spec)
    names = contact_names(spec, side)
    out: list[str] = []
    for level, members in enumerate(spec.etageidx):
        if len(members) == 1:
            out.append(names[int(members[0]) - 1])
        elif rule.style == "numeric":
            numbers = [rule.start(side) + int(c) - 1 for c in members]
            out.append(f"K{min(numbers)}-{max(numbers)}")
        else:
            out.append(f"K{rule.start(side) + level}")
    return tuple(out)
