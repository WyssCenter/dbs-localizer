# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Reconstruction I/O: Reco v2 JSON (specs/05 §8)."""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

SIDE_RIGHT = "1"
SIDE_LEFT = "2"
SIDE_NAMES: dict[str, str] = {SIDE_RIGHT: "right", SIDE_LEFT: "left"}
SIDE_KEYS_CONVENTION = "1=right,2=left"


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


def read_recon_json_v2(path: Path | str) -> RecoFileV2:
    d = json.loads(Path(path).read_text(encoding="utf-8"))
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
    p.write_text(json.dumps(data.to_dict(), indent=2) + "\n", encoding="utf-8")


__all__ = [
    "RecoFileV2",
    "read_recon_json_v2",
    "write_recon_json_v2",
]
