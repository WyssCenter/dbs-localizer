# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Reconstruction I/O: Reco v2 JSON (specs/05 §8)."""

from __future__ import annotations

import json
import logging
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from dbsloc.electrodes.layout import (
    ElectrodeLayout,
    UnsupportedElectrodeLayout,
    layout_from_spec,
)
from dbsloc.electrodes.registry import get_elspec, list_models

SIDE_RIGHT = "1"
SIDE_LEFT = "2"
SIDE_NAMES: dict[str, str] = {SIDE_RIGHT: "right", SIDE_LEFT: "left"}
SIDE_KEYS_CONVENTION = "1=right,2=left"
TAIL_FIDUCIAL_FOURTH_LEVEL = "fourth_level"
_log = logging.getLogger(__name__)
MARKER_BLOCKS = ("native", "mni", "reference", "scrf")
TRAJECTORY_POINTS = 50
TRAJECTORY_PAST_HEAD_MM = 5.0


@dataclass
class RecoFileV2:
    """
    Specs/05 §8 **Reco** JSON (MNI + optional native / scrf) — v2.

    ``mni`` / ``native`` / ``props`` keys: :data:`SIDE_RIGHT` (``"1"``) is the right
    hemisphere and :data:`SIDE_LEFT` (``"2"``) the left, the `specs/09` order. Writers record
    :data:`SIDE_KEYS_CONVENTION` in ``meta.side_keys``. Until 2026-10-04 Wyss DBS Localizer wrote the
    opposite order and no ``meta.side_keys``; readers resolve hemispheres through
    :func:`side_hemispheres` / :func:`side_key_for`, which handle both.
    """

    subj_id: str
    method: str
    mni: dict[str, Any]
    version: int = 2
    native: dict[str, Any] | None = None
    scrf: dict[str, Any] | None = None
    acpc: dict[str, Any] | None = None
    props: dict[str, Any] | None = None
    meta: dict[str, Any] = field(default_factory=dict)
    reference: dict[str, Any] | None = None

    @staticmethod
    def from_spec(
        subj_id: str,
        method: str,
        sides: dict[str, Any],
    ) -> RecoFileV2:
        return RecoFileV2(subj_id=subj_id, method=method, mni=sides, version=2)

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "version": self.version,
            "subj_id": self.subj_id,
            "method": self.method,
            "mni": self.mni,
        }
        if self.native is not None:
            d["native"] = self.native
        if self.scrf is not None:
            d["scrf"] = self.scrf
        if self.acpc is not None:
            d["acpc"] = self.acpc
        if self.props is not None:
            d["props"] = self.props
        if self.meta:
            d["meta"] = self.meta
        if self.reference is not None:
            d["reference"] = self.reference
        return d

    @staticmethod
    def from_dict(d: dict[str, Any]) -> RecoFileV2:
        v = d.get("version", 1)
        if v != 2:
            raise ValueError("RecoFileV2: expected version 2")
        return RecoFileV2(
            subj_id=str(d.get("subj_id", "")),
            method=str(d.get("method", "")),
            mni=dict(d.get("mni") or {}),
            version=2,
            native=dict(d["native"]) if d.get("native") else None,
            scrf=dict(d["scrf"]) if d.get("scrf") else None,
            acpc=dict(d["acpc"]) if d.get("acpc") else None,
            props=dict(d["props"]) if d.get("props") else None,
            meta=dict(d.get("meta", {})),
            reference=dict(d["reference"]) if d.get("reference") else None,
        )


def tail_conversion_warning(side: str) -> str:
    return (
        f"dbsloc: warning: reconstruction: side {side}: tail marker moved from the "
        "last to the 4th contact level (file written before 2026-11-14)"
    )


def _xyz(v: Any) -> list[float] | None:
    if not isinstance(v, list) or len(v) != 3:
        return None
    if not all(isinstance(c, int | float) and not isinstance(c, bool) for c in v):
        return None
    p = [float(c) for c in v]
    return p if all(math.isfinite(c) for c in p) else None


def _dict(v: Any) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _head_tail(
    d: dict[str, Any], block: str, key: str
) -> tuple[list[float], list[float]] | None:
    side = _dict(d.get(block)).get(key)
    if not isinstance(side, dict) or not isinstance(side.get("markers"), dict):
        return None
    head = _xyz(side["markers"].get("head"))
    tail = _xyz(side["markers"].get("tail"))
    return (head, tail) if head is not None and tail is not None else None


def _layout_by_name(name: str) -> ElectrodeLayout | None:
    want = name.strip().lower()
    if not want:
        return None
    for elmodel, matfname in list_models():
        if elmodel.lower() == want or matfname.lower() == want:
            try:
                return layout_from_spec(get_elspec(matfname))
            except KeyError, UnsupportedElectrodeLayout:
                return None
    return None


def _side_layout(
    d: dict[str, Any], key: str, fallback_model: str | None
) -> ElectrodeLayout | None:
    name = _dict(_dict(d.get("props")).get(key)).get("elmodel")
    if not isinstance(name, str):
        name = _dict(d.get("meta")).get("elmodel")
    if not isinstance(name, str):
        name = fallback_model
    if not isinstance(name, str):
        return None
    return _layout_by_name(name)


def trajectory(
    head: list[float], tail: list[float], layout: ElectrodeLayout
) -> list[list[float]] | None:
    d = [t - h for h, t in zip(head, tail, strict=True)]
    n = math.sqrt(sum(c * c for c in d))
    if not (math.isfinite(n) and n > 1e-6):
        return None
    u = [c / n for c in d]
    k = 1.5 if layout.n_levels > 8 else 2.0
    ext = k * max(layout.array_span_mm, 0.0)
    a1 = [h - TRAJECTORY_PAST_HEAD_MM * c for h, c in zip(head, u, strict=True)]
    a2 = [t + ext * c for t, c in zip(tail, u, strict=True)]
    last = TRAJECTORY_POINTS - 1
    return [
        [p + (q - p) * i / last for p, q in zip(a1, a2, strict=True)]
        for i in range(TRAJECTORY_POINTS)
    ]


def convert_legacy_tail(
    d: dict[str, Any], fallback_model: str | None = None
) -> list[str]:
    if "tail_fiducial" in _dict(d.get("meta")):
        return []
    keys: list[str] = []
    for block in ("native", "mni"):
        sides = d.get(block)
        if isinstance(sides, dict):
            for k, v in sides.items():
                if isinstance(v, dict) and k not in keys:
                    keys.append(k)
    out: list[str] = []
    for key in keys:
        layout = _side_layout(d, key, fallback_model)
        if layout is None or layout.n_levels <= 4:
            continue
        last = layout.level_z_mm[-1] - layout.level_z_mm[0]
        fiducial = layout.fiducial_span_mm
        if not (math.isfinite(last) and last > 1e-9 and math.isfinite(fiducial)):
            continue
        decide = _head_tail(d, "native", key) or _head_tail(d, "mni", key)
        if decide is None:
            continue
        dist = math.dist(decide[0], decide[1])
        if abs(dist - last) >= abs(dist - fiducial):
            continue
        ratio = fiducial / last
        for block in MARKER_BLOCKS:
            ht = _head_tail(d, block, key)
            if ht is None:
                continue
            head, tail = ht
            new_tail = [h + ratio * (t - h) for h, t in zip(head, tail, strict=True)]
            side = d[block][key]
            side["markers"]["tail"] = new_tail
            if "trajectory" in side:
                traj = trajectory(head, new_tail, layout)
                if traj is not None:
                    side["trajectory"] = traj
        out.append(tail_conversion_warning(key))
    return out


def read_recon_json_v2(path: Path | str) -> RecoFileV2:
    d = json.loads(Path(path).read_text(encoding="utf-8"))
    if isinstance(d, dict):
        for w in convert_legacy_tail(d):
            _log.warning(w)
    return RecoFileV2.from_dict(d)


def _side_mean_x(side: Any) -> float | None:
    if not isinstance(side, dict):
        return None
    mk = side.get("markers")
    if isinstance(mk, dict) and "head" in mk and "tail" in mk:
        return 0.5 * (float(mk["head"][0]) + float(mk["tail"][0]))
    for k in ("trajectory", "coords_mm"):
        pts = side.get(k)
        if isinstance(pts, list) and pts:
            return sum(float(p[0]) for p in pts) / len(pts)
    return None


def side_hemispheres(reco: RecoFileV2, space: str = "mni") -> dict[str, str]:
    """
    Reco v2 side key → ``"right"`` / ``"left"`` for the sides present in ``space``.

    A file that declares :data:`SIDE_KEYS_CONVENTION` in ``meta.side_keys`` is read by key.
    A file without it predates the 2026-10-04 key flip and is read by mean *x* of each side:
    of two sides the larger is right, a single side goes by its sign. ``ValueError`` for an
    unknown declared convention, two sides at the same mean *x*, or more than two sides. The
    Rust viewer (`crates/dbsloc-viz/src/reco_json.rs`) applies the same rule.
    """
    block = {
        k: v
        for k, v in (getattr(reco, space, None) or {}).items()
        if isinstance(v, dict)
    }
    declared = (reco.meta or {}).get("side_keys")
    if declared is not None:
        if declared != SIDE_KEYS_CONVENTION:
            raise ValueError(f"meta.side_keys {declared!r} is not a known convention")
        return {k: SIDE_NAMES[k] for k in block if k in SIDE_NAMES}
    xs = [(k, x) for k, v in block.items() if (x := _side_mean_x(v)) is not None]
    if len(xs) > 2:
        raise ValueError(f"expected at most two sides in {space}, found {len(xs)}")
    if len(xs) == 1:
        k, x = xs[0]
        return {k: "right" if x > 0.0 else "left"}
    if len(xs) == 2:
        (ka, xa), (kb, xb) = xs
        if xa == xb:
            raise ValueError(
                f"both {space} sides have the same mean x ({xa:.2f} mm); cannot tell the "
                "hemispheres apart"
            )
        return {ka: "right", kb: "left"} if xa > xb else {ka: "left", kb: "right"}
    return {}


def parse_hemisphere(side: str) -> str:
    """``"1"``/``"r"``/``"right"`` → ``"right"``, ``"2"``/``"l"``/``"left"`` → ``"left"``."""
    s = str(side).strip().lower()
    if s in ("1", "r", "right"):
        return "right"
    if s in ("2", "l", "left"):
        return "left"
    raise ValueError(f"side {side!r}: expected 1/right or 2/left")


def side_key_for(reco: RecoFileV2, side: str, space: str = "mni") -> str:
    """The key of hemisphere ``side`` (see :func:`parse_hemisphere`) in ``space``."""
    want = parse_hemisphere(side)
    for k, h in side_hemispheres(reco, space).items():
        if h == want:
            return k
    raise ValueError(f"no {want} side in the {space} block")


def write_recon_json_v2(path: Path | str, data: RecoFileV2) -> None:
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    d = data.to_dict()
    d["meta"] = {**d.get("meta", {}), "tail_fiducial": TAIL_FIDUCIAL_FOURTH_LEVEL}
    p.write_text(json.dumps(d, indent=2) + "\n", encoding="utf-8")


__all__ = [
    "TAIL_FIDUCIAL_FOURTH_LEVEL",
    "RecoFileV2",
    "convert_legacy_tail",
    "read_recon_json_v2",
    "tail_conversion_warning",
    "trajectory",
    "write_recon_json_v2",
]
