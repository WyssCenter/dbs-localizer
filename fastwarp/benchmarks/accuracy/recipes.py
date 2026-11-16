# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""The registration recipes of the benchmark, for FastWarp and as ``antsRegistration`` argv.

``cohort`` is the FastWarp default (``fastwarp.cohort_recipe(1)``), the recipe the README
timings use. ``syn-sh`` is what ``antsRegistrationSyN.sh -t s`` of ANTs 2.6.5 runs on an image
below its "large image" size: the cohort recipe with the SyN gradient step 0.2 and CC radius 2.
Both engines run the same recipe: FastWarp from the dict, ANTs from :func:`ants_argv`.
"""

from __future__ import annotations

import copy
from typing import Any

_LINEAR = {
    "gradient_step": 0.1,
    "iterations": [1000, 500, 250, 100],
    "convergence_threshold": 1e-06,
    "convergence_window": 10,
    "shrink_factors": [8, 4, 2, 1],
    "smoothing_sigmas": [3.0, 2.0, 1.0, 0.0],
    "sigma_units": "vox",
    "bins": 32,
    "sampling_percentage": 0.25,
    "seed": 20260910,
}

_SYN = {
    "gradient_step": 0.1,
    "update_field_variance": 3.0,
    "total_field_variance": 0.0,
    "cc_radius": 4,
    "channel_weights": [1.0],
    "iterations": [100, 70, 50, 20],
    "convergence_threshold": 1e-06,
    "convergence_window": 10,
    "shrink_factors": [8, 4, 2, 1],
    "smoothing_sigmas": [3.0, 2.0, 1.0, 0.0],
    "sigma_units": "vox",
}

RECIPES: dict[str, dict[str, dict[str, Any]]] = {
    "cohort": {"rigid": _LINEAR, "affine": _LINEAR, "syn": _SYN},
    "syn-sh": {
        "rigid": _LINEAR,
        "affine": _LINEAR,
        "syn": {**_SYN, "gradient_step": 0.2, "cc_radius": 2},
    },
}

#: The intensity quantiles both engines winsorize to.
WINSORIZE = (0.005, 0.995)


def recipe(name: str) -> dict[str, dict[str, Any]]:
    """A deep copy of the named recipe (single channel, T1w)."""
    if name not in RECIPES:
        raise ValueError(f"unknown recipe {name!r}; choose from {sorted(RECIPES)}")
    return copy.deepcopy(RECIPES[name])


def _num(v: float | int) -> str:
    f = float(v)
    return str(int(f)) if f.is_integer() and abs(f) < 1e15 else repr(f)


def _x(values: list) -> str:
    return "x".join(_num(v) for v in values)


def _check_path(p: str) -> str:
    if any(c in p for c in ",[]"):
        raise ValueError(f"antsRegistration cannot take a path with ',', '[' or ']': {p}")
    return p


def _schedule(stage: dict[str, Any]) -> list[str]:
    return [
        "--convergence",
        f"[{_x(stage['iterations'])},{_num(stage['convergence_threshold'])},"
        f"{int(stage['convergence_window'])}]",
        "--shrink-factors",
        _x(stage["shrink_factors"]),
        "--smoothing-sigmas",
        f"{_x(stage['smoothing_sigmas'])}{stage['sigma_units']}",
    ]


def _head(fixed: str, moving: str, out_prefix: str, initial: str | None) -> list[str]:
    start = f"[{fixed},{moving},1]" if initial is None else f"[{_check_path(initial)}]"
    return [
        "--dimensionality",
        "3",
        "--output",
        f"[{out_prefix}]",
        "--winsorize-image-intensities",
        f"[{_num(WINSORIZE[0])},{_num(WINSORIZE[1])}]",
        "--use-histogram-matching",
        "0",
        "--initial-moving-transform",
        start,
    ]


def _linear_stage(transform: str, s: dict[str, Any], fixed: str, moving: str) -> list[str]:
    return [
        "--transform",
        f"{transform}[{_num(s['gradient_step'])}]",
        "--metric",
        f"MI[{fixed},{moving},1,{int(s['bins'])},Regular,{_num(s['sampling_percentage'])}]",
        *_schedule(s),
    ]


def ants_argv(
    rec: dict[str, dict[str, Any]], fixed: str, moving: str, out_prefix: str
) -> list[str]:
    """The ``antsRegistration`` arguments (no program name) that run ``rec`` on one channel and
    write ``<out_prefix>Composite.h5`` / ``<out_prefix>InverseComposite.h5``.

    Mirrors FastWarp: centre-of-mass start, Rigid and Affine on ``MI[bins, Regular, sampling]``,
    SyN on ``CC[radius]``, winsorizing ``[0.005, 0.995]``, no histogram matching, the rigid
    stage's seed as ``--random-seed``. Precision is ANTs' default (double). ``--verbose 1``
    makes ANTs print each stage's time, which the benchmark reads from the log.
    """
    fixed, moving, out_prefix = _check_path(fixed), _check_path(moving), _check_path(out_prefix)
    weights = rec["syn"]["channel_weights"]
    if len(weights) != 1:
        raise ValueError("the benchmark registers one channel (T1w)")
    argv = _head(fixed, moving, out_prefix, None)
    for name, transform in (("rigid", "Rigid"), ("affine", "Affine")):
        argv += _linear_stage(transform, rec[name], fixed, moving)
    s = rec["syn"]
    argv += [
        "--transform",
        f"SyN[{_num(s['gradient_step'])},{_num(s['update_field_variance'])},"
        f"{_num(s['total_field_variance'])}]",
        "--metric",
        f"CC[{fixed},{moving},{_num(weights[0])},{int(s['cc_radius'])}]",
        *_schedule(s),
        "--write-composite-transform",
        "1",
        "--random-seed",
        str(int(rec["rigid"]["seed"])),
        "--verbose",
        "1",
    ]
    return argv


def ants_linear_argv(
    stage: dict[str, Any],
    transform: str,
    fixed: str,
    moving: str,
    out_prefix: str,
    initial: str | None = None,
) -> list[str]:
    """The ``antsRegistration`` arguments of one linear stage (``transform`` ``"rigid"`` or
    ``"affine"``) as :func:`ants_argv` runs it, writing ``<out_prefix>0GenericAffine.mat`` (the
    start and the fit collapsed into one affine). ``initial``: a ``.mat`` file to start from;
    ``None`` starts from the centres of intensity mass."""
    names = {"rigid": "Rigid", "affine": "Affine"}
    if transform not in names:
        raise ValueError(f"transform must be 'rigid' or 'affine', not {transform!r}")
    fixed, moving, out_prefix = _check_path(fixed), _check_path(moving), _check_path(out_prefix)
    return [
        *_head(fixed, moving, out_prefix, initial),
        *_linear_stage(names[transform], stage, fixed, moving),
        "--random-seed",
        str(int(stage["seed"])),
        "--verbose",
        "1",
    ]
