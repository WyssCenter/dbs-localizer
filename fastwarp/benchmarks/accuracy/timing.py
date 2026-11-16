# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Timings of the registrations: the split into linear stages and SyN for both engines, and
summaries over pairs.

FastWarp reports ``linear_s``, ``syn_s`` and ``total_s`` in the result of ``register_syn``.
``antsRegistration --verbose 1`` prints ``Elapsed time (stage <n>): <s>`` after each stage and
``Total elapsed time: <s>`` at the end of the registration (ITK ``TimeProbe``, wall clock); a
stage's transform is the one named in the ``*** Running <transform> registration ***`` line
before it. ANTs' total covers the stages and their set-up, not reading the images or writing
the transforms; the process wall time (``wall_s``) covers everything.
"""

from __future__ import annotations

import json
import math
import re
from collections.abc import Iterable
from pathlib import Path
from typing import Any

import numpy as np

_RUNNING = re.compile(r"^\s*\*\*\* Running (\S+) registration")
_STAGE = re.compile(r"^\s*Elapsed time \(stage (\d+)\):\s*(\S+)\s*$")
_TOTAL = re.compile(r"^\s*Total elapsed time:\s*(\S+)\s*$")

#: The part of the name in ``*** Running <name> registration`` that marks a SyN stage.
_DEFORMABLE = ("SyN",)
#: Names in ``*** Running <name> registration`` of the linear stages.
_LINEAR = (
    "Euler3DTransform",
    "AffineTransform",
    "Similarity3DTransform",
    "TranslationTransform",
    "QuaternionRigidTransform",
    "VersorRigid3DTransform",
    "Euler2DTransform",
    "Similarity2DTransform",
)


def _number(text: str) -> float | None:
    try:
        v = float(text)
    except ValueError:
        return None
    return v if math.isfinite(v) else None


def parse_ants_log(text: str) -> dict[str, Any]:
    """The stage times and the total of an ``antsRegistration --verbose 1`` log.

    Returns ``{"stages": [{"stage", "transform", "elapsed_s"}], "total_s"}``; ``total_s`` is
    ``None`` when the log has no total line (an interrupted or non-verbose run).
    """
    stages: list[dict[str, Any]] = []
    running: str | None = None
    total: float | None = None
    for line in text.splitlines():
        if m := _RUNNING.match(line):
            running = m.group(1)
        elif m := _STAGE.match(line):
            stages.append(
                {"stage": int(m.group(1)), "transform": running, "elapsed_s": _number(m.group(2))}
            )
            running = None
        elif m := _TOTAL.match(line):
            total = _number(m.group(1))
    return {"stages": stages, "total_s": total}


def is_linear(transform: str | None) -> bool | None:
    """Whether an ANTs stage transform name is linear (``None`` when unknown)."""
    if transform is None:
        return None
    if transform in _LINEAR:
        return True
    if any(k in transform for k in _DEFORMABLE):
        return False
    return None


def ants_registration_timings(text: str, n_stages: int | None = None) -> dict[str, Any]:
    """``linear_s`` (sum of the linear stages), ``syn_s`` (the deformable stages) and
    ``total_s`` from an ``antsRegistration`` log; a part that the log does not give
    completely (a stage missing, a time or a transform name unreadable, a stage count other
    than ``n_stages``) is ``None``, with the reason in ``note``."""
    parsed = parse_ants_log(text)
    stages = parsed["stages"]
    out: dict[str, Any] = {
        "linear_s": None,
        "syn_s": None,
        "total_s": parsed["total_s"],
        "stages": stages,
        "source": "antsRegistration --verbose 1 log",
    }
    problems = []
    if not stages:
        problems.append("no 'Elapsed time (stage n)' lines (not a --verbose 1 log?)")
    if n_stages is not None and stages and len(stages) != n_stages:
        problems.append(f"{len(stages)} stage times for {n_stages} stages")
    if [s["stage"] for s in stages] != list(range(len(stages))):
        problems.append("stage numbers are not 0, 1, 2, ...")
    if any(s["elapsed_s"] is None or is_linear(s["transform"]) is None for s in stages):
        problems.append("a stage without a readable time or transform")
    if not problems:
        linear = [s["elapsed_s"] for s in stages if is_linear(s["transform"])]
        deform = [s["elapsed_s"] for s in stages if not is_linear(s["transform"])]
        out["linear_s"] = float(sum(linear)) if linear else None
        out["syn_s"] = float(sum(deform)) if deform else None
    if parsed["total_s"] is None:
        problems.append("no 'Total elapsed time' line")
    if problems:
        out["note"] = "; ".join(problems)
    return out


def fastwarp_registration_timings(info: dict[str, Any] | None) -> dict[str, Any]:
    """The engine's own ``linear_s``, ``syn_s`` and ``total_s`` from a ``register_syn``
    result (``None`` where missing)."""
    info = info or {}

    def get(k: str) -> float | None:
        v = info.get(k)
        return float(v) if isinstance(v, (int, float)) else None

    return {
        "linear_s": get("linear_s"),
        "syn_s": get("syn_s"),
        "total_s": get("total_s"),
        "source": "register_syn result",
    }


def registration_timings(record: dict[str, Any], folder: Path | None = None) -> dict[str, Any]:
    """The ``timings`` of a registration ``run.json`` (with the process ``wall_s``); for records
    written before timings were recorded, derived from FastWarp's ``info`` or from the engine
    folder's ``ants.log``."""
    t = record.get("timings")
    if t is None:
        if record.get("engine") == "fastwarp":
            t = fastwarp_registration_timings(record.get("info"))
        elif folder is not None and (folder / "ants.log").is_file():
            text = (folder / "ants.log").read_text(encoding="utf-8", errors="replace")
            t = ants_registration_timings(text, n_stages=3)
        else:
            t = {"linear_s": None, "syn_s": None, "total_s": None, "note": "no timings"}
    out = dict(t)
    out["wall_s"] = record.get("wall_s")
    return out


def describe(values: Iterable[Any]) -> dict[str, Any]:
    """``n``, median, quartiles, min and max of the finite numbers in ``values``."""
    v = np.array(
        [float(x) for x in values if isinstance(x, (int, float)) and math.isfinite(float(x))]
    )
    if not v.size:
        return {"n": 0, "median": math.nan, "q1": math.nan, "q3": math.nan}
    q1, med, q3 = np.percentile(v, [25, 50, 75])
    return {
        "n": int(v.size),
        "median": float(med),
        "q1": float(q1),
        "q3": float(q3),
        "min": float(v.min()),
        "max": float(v.max()),
    }


def spread(d: dict[str, Any], digits: int = 1) -> str:
    """``median (q1–q3)`` of a :func:`describe` result, or ``n/a``."""
    if not d.get("n"):
        return "n/a"
    if d["n"] == 1:
        return f"{d['median']:.{digits}f}"
    return f"{d['median']:.{digits}f} ({d['q1']:.{digits}f}–{d['q3']:.{digits}f})"


def thread_label(env: dict[str, Any] | None) -> Any:
    """The thread setting of an ``environment`` record: its ``threads`` (``"all cores"`` or N);
    for records made before that field, what their ITK and FastWarp settings were (``None``
    when neither was recorded)."""
    env = env or {}
    if "threads" in env:
        return env["threads"]
    itk, rayon = env.get("itk_threads"), env.get("rayon_threads")
    if itk is None and rayon is None:
        return None
    if itk is not None and rayon is not None and str(rayon) == str(itk):
        return itk
    return f"ANTs {itk or 'all cores'}, FastWarp {rayon or 'all cores'}"


def fingerprint(env: dict[str, Any] | None) -> dict[str, Any]:
    """The machine and software fields of an ``environment`` record that a timing depends on."""
    env = env or {}
    gpu = env.get("gpu") or {}
    return {
        "gpu": gpu.get("name", gpu.get("error")),
        "gpu_driver": gpu.get("driver"),
        "gpu_backend": gpu.get("backend"),
        "cpu": env.get("cpu"),
        "threads": thread_label(env),
        "fastwarp": env.get("fastwarp"),
        "ants": env.get("ants_version") or env.get("ants_program"),
    }


def read_records(paths: Iterable[Path]) -> list[tuple[Path, dict[str, Any]]]:
    """``(path, record)`` of every readable JSON file in ``paths``, in sorted order."""
    out = []
    for p in sorted(paths):
        try:
            out.append((p, json.loads(p.read_text(encoding="utf-8"))))
        except (OSError, json.JSONDecodeError):
            continue
    return out


def registration_rows(
    work: Path, recipe_name: str, recipe: dict[str, Any] | None = None
) -> list[dict[str, Any]]:
    """One row per finished registration in ``work/*/<recipe_name>/`` (made with ``recipe``,
    if given): dataset, grid, pair, engine, ``linear_s``, ``syn_s``, ``total_s``, ``wall_s``
    and the machine fingerprint."""
    rows = []
    for path, rec in read_records(work.glob(f"*/{recipe_name}/*/*/run.json")):
        if rec.get("status") != "ok" or rec.get("kind", "registration") != "registration":
            continue
        if recipe is not None and rec.get("recipe") != recipe:
            continue
        engine = rec.get("engine")
        if engine not in ("fastwarp", "ants"):
            continue
        t = registration_timings(rec, path.parent)
        rows.append(
            {
                "dataset": path.parents[3].name,
                "iso_mm": rec.get("iso_mm"),
                "pair": path.parents[1].name,
                "engine": engine,
                "read_s": rec.get("read_s"),
                "linear_s": t.get("linear_s"),
                "syn_s": t.get("syn_s"),
                "total_s": t.get("total_s"),
                "wall_s": t.get("wall_s"),
                "timing_note": t.get("note", ""),
                **fingerprint(rec.get("environment")),
            }
        )
    return rows


def summary_rows(
    rows: list[dict[str, Any]], keys: tuple[str, ...], fields: tuple[str, ...]
) -> list[dict[str, Any]]:
    """Per distinct value of ``keys``: :func:`describe` of each of ``fields`` (as
    ``<field>_<stat>`` columns) and the number of rows."""
    groups: dict[tuple, list[dict[str, Any]]] = {}
    for r in rows:
        groups.setdefault(tuple(r.get(k) for k in keys), []).append(r)
    out = []
    for g in sorted(groups, key=lambda t: tuple(str(x) for x in t)):
        sel = groups[g]
        row: dict[str, Any] = dict(zip(keys, g, strict=True))
        row["rows"] = len(sel)
        for f in fields:
            for stat, v in describe(r.get(f) for r in sel).items():
                row[f"{f}_{stat}"] = v
        out.append(row)
    return out


def stats_of(row: dict[str, Any], field: str) -> dict[str, Any]:
    """The :func:`describe` result of ``field`` stored in a :func:`summary_rows` row."""
    return {k: row.get(f"{field}_{k}", math.nan) for k in ("n", "median", "q1", "q3", "min", "max")}


def ratio(a: float, b: float) -> float:
    """``a / b``, NaN when either is missing or ``b`` is 0."""
    if not all(isinstance(x, (int, float)) and math.isfinite(x) for x in (a, b)) or b == 0:
        return math.nan
    return float(a) / float(b)


def machines(tables: dict[str, list[dict[str, Any]]]) -> list[tuple[dict[str, Any], int]]:
    """Distinct machine fingerprints of the records behind timing rows (``{table: rows}``, rows
    carrying :func:`fingerprint`'s fields), with how many records each has."""
    keys = tuple(fingerprint(None))
    per_record: dict[tuple, dict[str, Any]] = {}
    for table, rows in tables.items():
        for r in rows:
            per_record.setdefault(
                (table, r.get("dataset"), r.get("iso_mm"), r.get("pair"), r.get("engine")),
                {k: r.get(k) for k in keys},
            )
    seen: dict[str, tuple[dict[str, Any], int]] = {}
    for fp in per_record.values():
        key = json.dumps(fp, sort_keys=True, default=str)
        prev = seen.get(key)
        seen[key] = (fp, (prev[1] if prev else 0) + 1)
    return list(seen.values())
