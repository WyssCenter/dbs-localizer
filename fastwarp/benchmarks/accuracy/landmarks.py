# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""3D Slicer markups (``.fcsv``) landmark files, as the AFIDs datasets ship them."""

from __future__ import annotations

import csv
import io
from pathlib import Path

import numpy as np

_RAS = {"0", "RAS"}
_LPS = {"1", "LPS"}


def parse_fcsv(text: str) -> dict[str, tuple[str, np.ndarray]]:
    """``{label: (description, point)}`` with points in ITK (LPS) millimetres.

    Reads the ``# CoordinateSystem`` header (``0``/``RAS`` or ``1``/``LPS``; RAS points are
    converted) and the ``# columns`` header. Points are keyed by the ``label`` column, which
    must be unique.
    """
    system = "0"
    columns = ["id", "x", "y", "z", "ow", "ox", "oy", "oz", "vis", "sel", "lock", "label", "desc"]
    rows = []
    for line in text.splitlines():
        if line.startswith("#"):
            key, _, value = line[1:].partition("=")
            key, value = key.strip().lower(), value.strip()
            if key == "coordinatesystem":
                system = value.upper()
            elif key == "columns":
                columns = [c.strip() for c in value.split(",")]
        elif line.strip():
            rows.append(line)
    if system not in _RAS | _LPS:
        raise ValueError(f"unsupported fcsv coordinate system {system!r}")
    flip = np.array([-1.0, -1.0, 1.0]) if system in _RAS else np.ones(3)
    out: dict[str, tuple[str, np.ndarray]] = {}
    for rec in csv.reader(io.StringIO("\n".join(rows))):
        row = dict(zip(columns, rec, strict=False))
        label = row.get("label", "").strip()
        if not label:
            raise ValueError("an fcsv row has no label")
        if label in out:
            raise ValueError(f"duplicate fcsv label {label!r}")
        point = np.array([float(row["x"]), float(row["y"]), float(row["z"])]) * flip
        out[label] = (row.get("desc", "").strip(), point)
    return out


def read_fcsv(path: str | Path) -> dict[str, tuple[str, np.ndarray]]:
    """:func:`parse_fcsv` of a file."""
    return parse_fcsv(Path(path).read_text(encoding="utf-8"))


def matched(
    a: dict[str, tuple[str, np.ndarray]], b: dict[str, tuple[str, np.ndarray]]
) -> tuple[list[str], list[str], np.ndarray, np.ndarray]:
    """The labels both sets have, their descriptions (from ``a``) and the two N x 3 arrays."""
    labels = sorted(set(a) & set(b), key=lambda s: (len(s), s))
    if not labels:
        return [], [], np.empty((0, 3)), np.empty((0, 3))
    return (
        labels,
        [a[k][0] for k in labels],
        np.stack([a[k][1] for k in labels]),
        np.stack([b[k][1] for k in labels]),
    )
