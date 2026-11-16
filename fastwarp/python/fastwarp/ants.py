# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""ANTsPy-compatible registration and resampling: a drop-in subset of ``ants``.

:func:`registration` and :func:`apply_transforms` take the arguments of ANTsPy's
``ants.registration`` and ``ants.apply_transforms`` (ANTsPy 0.6.3) and return what they return.
Each call builds the ``antsRegistration`` or ``antsApplyTransforms`` argument list ANTsPy would
build and runs it with the ANTs compatibility mode of the ``fastwarp`` command line
(``fastwarp ants-registration``, ``fastwarp ants-apply-transforms``), with the images held in
memory. The registration runs on the GPU; resampling runs on the CPU.

Anything that would change the result and that FastWarp does not support raises
:class:`UnsupportedArgument` before any work. Arguments that ANTsPy itself ignores for the chosen
``type_of_transform`` are ignored here too, with a :class:`UserWarning`. Nothing falls back to
ANTsPy unless ``fallback="ants"`` is given.

ANTsPy (the ``antspyx`` package) must be installed: the functions take and return
``ANTsImage`` objects. Importing this module does not import ANTsPy, and an argument list
(``registration([...], None)``) runs without it.
"""

from __future__ import annotations

import contextlib
import math
import os
import sys
import tempfile
import warnings
from collections.abc import Sequence
from os import PathLike, fspath
from typing import Any

import numpy as np

from . import _core
from ._api import _grid_args, _image_args, _prepared_prefix
from ._core import CancelToken, GpuCapacityError, GpuUnavailableError, UnsupportedArgument

UnsupportedArgument.__module__ = "fastwarp.ants"

__all__ = ["UnsupportedArgument", "apply_transforms", "registration"]

_REG = "registration"
_APPLY = "apply_transforms"

_TV_TYPES = tuple(f"TV[{n}]" for n in range(1, 9))

_ALLOWABLE = {
    "SyNBold",
    "SyNBoldAff",
    "ElasticSyN",
    "Elastic",
    "SyN",
    "SyNRA",
    "SyNOnly",
    "SyNAggro",
    "SyNCC",
    "TRSAA",
    "SyNabp",
    "SyNLessAggro",
    *_TV_TYPES,
    "TVMSQ",
    "TVMSQC",
    "Rigid",
    "Similarity",
    "Translation",
    "Affine",
    "AffineFast",
    "BOLDAffine",
    "QuickRigid",
    "DenseRigid",
    "BOLDRigid",
}

_DEFORMABLE_ONLY = {
    "SyNOnly",
    "antsRegistrationSyN[so]",
    "antsRegistrationSyNQuick[so]",
    "antsRegistrationSyNRepro[so]",
    "antsRegistrationSyNQuickRepro[so]",
    "antsRegistrationSyN[bo]",
    "antsRegistrationSyNQuick[bo]",
    "antsRegistrationSyNRepro[bo]",
    "antsRegistrationSyNQuickRepro[bo]",
    "TVMSQ",
    "TVMSQC",
    *_TV_TYPES,
}

_LINEAR = {"Rigid", "Affine", "AffineFast", "BOLDAffine", "QuickRigid", "DenseRigid", "BOLDRigid"}
_MYSYN = {
    "SyN",
    "SyNRA",
    "SyNOnly",
    "SyNAggro",
    "SyNLessAggro",
    "ElasticSyN",
    "SyNBold",
    "SyNBoldAff",
}
_AFF_METRIC = _LINEAR | (_MYSYN - {"SyNOnly"}) | {"SyNCC"}
_EARLY_MASKS = (_MYSYN - {"SyNOnly"}) | {"SyNCC", "SyNabp"}

_REFUSED_TYPES = {
    "Translation": "FastWarp has no Translation transform",
    "Similarity": "FastWarp has no Similarity transform",
    "TRSAA": "FastWarp has no Translation or Similarity transform",
    "Elastic": "FastWarp has no GaussianDisplacementField transform",
    "TVMSQ": "FastWarp has no time-varying velocity field transform",
    "TVMSQC": "FastWarp has no time-varying velocity field transform",
    **{t: "FastWarp has no time-varying velocity field transform" for t in _TV_TYPES},
}

_SCRIPT_SUBTYPES = ("t", "r", "a", "s", "sr", "so", "b", "br", "bo")

_DEFAULTS: dict[str, Any] = {
    "type_of_transform": "SyN",
    "initial_transform": None,
    "outprefix": "",
    "mask": None,
    "moving_mask": None,
    "mask_all_stages": False,
    "grad_step": 0.2,
    "flow_sigma": 3,
    "total_sigma": 0,
    "aff_metric": "mattes",
    "aff_sampling": 32,
    "aff_random_sampling_rate": 0.2,
    "syn_metric": "mattes",
    "syn_sampling": 32,
    "reg_iterations": (40, 20, 0),
    "aff_iterations": (2100, 1200, 1200, 10),
    "aff_shrink_factors": (6, 4, 2, 1),
    "aff_smoothing_sigmas": (3, 2, 1, 0),
    "write_composite_transform": False,
    "verbose": False,
    "multivariate_extras": None,
    "restrict_transformation": None,
    "smoothing_in_mm": False,
    "singleprecision": True,
    "use_legacy_histogram_matching": False,
}

_ACCEPTED_INTERPOLATORS = {
    "linear",
    "nearestNeighbor",
    "multiLabel",
    "gaussian",
    "bSpline",
    "cosineWindowedSinc",
    "welchWindowedSinc",
    "hammingWindowedSinc",
    "lanczosWindowedSinc",
    "genericLabel",
}
_SUPPORTED_INTERPOLATORS = ("linear", "nearestNeighbor", "lanczosWindowedSinc")

_FIXED = "<fixed>.nii"
_MOVING = "<moving>.nii"
_WARPED_MOVING = "<warpedmovout>.nii"
_WARPED_FIXED = "<warpedfixout>.nii"
_MASK = "<mask>.nii"
_EXTRA_FIXED = "<extra_fixed_0>.nii"
_EXTRA_MOVING = "<extra_moving_0>.nii"

_LABELS = {
    _FIXED: "fixed",
    _MOVING: "moving",
    _WARPED_MOVING: "warpedmovout",
    _WARPED_FIXED: "warpedfixout",
    _MASK: "mask",
    _EXTRA_FIXED: "multivariate_extras[0][1]",
    _EXTRA_MOVING: "multivariate_extras[0][2]",
    "--initial-moving-transform": "initial_transform",
}


def _registration_names() -> dict[str, str]:
    """The names the argument list of :func:`registration` gives the images it holds in memory:
    no brackets or commas, which the argument syntax of ANTs reserves."""
    return {
        "fixed": _FIXED,
        "moving": _MOVING,
        "warpedmovout": _WARPED_MOVING,
        "warpedfixout": _WARPED_FIXED,
        "mask": _MASK,
        "extra_fixed": _EXTRA_FIXED,
        "extra_moving": _EXTRA_MOVING,
    }


def _readable(text: str) -> str:
    """``text`` with the in-memory image names replaced by the names of the arguments."""
    for name, label in _LABELS.items():
        text = text.replace(name, label)
    return text


def _ants() -> Any:
    try:
        import ants
    except ImportError as e:
        raise ImportError(
            "fastwarp.ants needs ANTsPy for its ANTsImage input and output; install it with "
            "'pip install antspyx' (in a FastWarp checkout: 'uv sync --group ants')"
        ) from e
    return ants


def _unsupported(function: str, argument: str, value: Any, reason: str) -> UnsupportedArgument:
    return UnsupportedArgument(
        f"fastwarp.ants.{function}: {argument}={value!r} is not supported: {reason}."
    )


def _same(a: Any, b: Any) -> bool:
    if isinstance(a, list):
        a = tuple(a)
    if isinstance(b, list):
        b = tuple(b)
    try:
        return bool(a == b)
    except (ValueError, TypeError):
        return False


def _ignored(function: str, argument: str, value: Any, why: str, stacklevel: int = 4) -> None:
    warnings.warn(
        f"fastwarp.ants.{function}: {argument}={value!r} has no effect {why} (ANTsPy ignores it "
        "too).",
        UserWarning,
        stacklevel=stacklevel,
    )


def _check_image(img: Any, what: str, function: str = _REG) -> None:
    if img.dimension != 3:
        raise _unsupported(
            function, f"{what}.dimension", img.dimension, "FastWarp handles 3-D images only"
        )
    if img.components != 1:
        raise _unsupported(
            function,
            f"{what}.components",
            img.components,
            "FastWarp handles scalar images only",
        )


def _join(values: Any) -> str:
    return "x".join(str(v) for v in values)


def _syn_schedule(reg_iterations: Any) -> tuple[str, str, str]:
    """``(iterations, smoothing sigmas, shrink factors)`` of the SyN stage as ANTsPy derives them
    from ``reg_iterations``."""
    n = len(reg_iterations)
    sigmas = np.arange(0, n)[::-1].astype("float32")
    shrink = (2**sigmas).astype("int")
    return _join(reg_iterations), "x".join(str(s)[0] for s in sigmas), _join(shrink)


def _script_type(t: str) -> dict[str, Any]:
    """The parts of an ``antsRegistrationSyN[...]`` type as ANTsPy 0.6.3 reads them."""
    quick = "Quick" in t
    repro = "Repro" in t
    subtype = "s"
    radius_given = None
    if "[" in t and "]" in t:
        subtype = t.split("[")[1].split("]")[0]
        if "," in subtype:
            parts = subtype.split(",")
            subtype = parts[0]
            if subtype not in ("b", "br", "bo", "s", "sr", "so"):
                raise ValueError("Extra parameters are only valid for 's' or 'b' SyN transforms.")
            radius_given = parts[1]
    return {"quick": quick, "repro": repro, "subtype": subtype, "radius_given": radius_given}


def _check_type(t: str) -> dict[str, Any] | None:
    """Refuses a ``type_of_transform`` FastWarp cannot run with these arguments; returns the
    parts of an ``antsRegistrationSyN[...]`` type (``None`` for the other types)."""
    if t in _REFUSED_TYPES:
        raise _unsupported(_REG, "type_of_transform", t, _REFUSED_TYPES[t])
    if t in _ALLOWABLE:
        return None
    script = _script_type(t)
    sub = script["subtype"]
    if script["repro"]:
        raise _unsupported(
            _REG, "type_of_transform", t, "FastWarp has no GC (global correlation) metric"
        )
    if sub not in _SCRIPT_SUBTYPES:
        raise ValueError(
            f"fastwarp.ants.registration: type_of_transform={t!r} runs no stage: the subtype in "
            f"the brackets is one of {', '.join(_SCRIPT_SUBTYPES)}"
        )
    if sub == "t":
        raise _unsupported(_REG, "type_of_transform", t, "FastWarp has no Translation transform")
    if sub in ("b", "br", "bo"):
        raise _unsupported(_REG, "type_of_transform", t, "FastWarp has no BSplineSyN transform")
    if script["quick"] and sub in ("s", "sr", "so"):
        raise _unsupported(
            _REG,
            "type_of_transform",
            t,
            "its SyN stage uses Mattes mutual information, which FastWarp's SyN stage does not "
            "have (it uses CC); use 'antsRegistrationSyN[" + sub + "]' or 'SyNCC'",
        )
    return script


def _syn_metric_check(a: dict[str, Any]) -> None:
    metric = a["syn_metric"]
    name = str(metric).lower()
    if name in ("mattes", "mi"):
        raise _unsupported(
            _REG,
            "syn_metric",
            metric,
            "FastWarp's SyN stage has no Mattes mutual information metric (type_of_transform="
            f"{a['type_of_transform']!r} uses syn_metric for its SyN stage); use "
            "type_of_transform='SyNCC', or syn_metric='CC' with syn_sampling (the CC radius) "
            "from 1 to 8",
        )
    if name in ("meansquares", "demons", "gc"):
        raise _unsupported(_REG, "syn_metric", metric, "FastWarp's SyN stage uses CC only")
    if name != "cc":
        raise ValueError(
            f"fastwarp.ants.registration: syn_metric={metric!r} is not an ANTs metric (CC, "
            "mattes, meansquares, demons)"
        )
    radius = a["syn_sampling"]
    if (
        isinstance(radius, bool)
        or not isinstance(radius, (int, np.integer))
        or not 1 <= radius <= 8
    ):
        raise _unsupported(
            _REG,
            "syn_sampling",
            radius,
            "with syn_metric='CC' it is the CC radius, and the GPU CC kernels support radius 1 "
            "to 8",
        )


def _aff_metric_check(a: dict[str, Any], t: str) -> None:
    metric = a["aff_metric"]
    name = str(metric).lower()
    if name in ("gc", "meansquares"):
        raise _unsupported(
            _REG, "aff_metric", metric, "FastWarp's linear stages use Mattes mutual information"
        )
    if name not in ("mattes", "mi"):
        raise ValueError(
            f"fastwarp.ants.registration: aff_metric={metric!r} is not an ANTs linear metric "
            "(mattes, GC, meansquares)"
        )
    bins = a["aff_sampling"]
    if isinstance(bins, bool) or not _same(bins, 32):
        raise _unsupported(
            _REG, "aff_sampling", bins, "the GPU Mattes kernels use 32 histogram bins"
        )
    rate = a["aff_random_sampling_rate"]
    if t != "DenseRigid":
        try:
            ok = 0.0 < float(rate) <= 1.0
        except (TypeError, ValueError):
            ok = False
        if not ok:
            raise ValueError(
                f"fastwarp.ants.registration: aff_random_sampling_rate={rate!r} must be in (0, 1]"
            )


def _extras_check(a: dict[str, Any], ants: Any, radius: int) -> None:
    extras = a["multivariate_extras"]
    if not isinstance(extras, (list, tuple)) or len(extras) != 1:
        raise _unsupported(
            _REG,
            "multivariate_extras",
            extras,
            "FastWarp's SyN stage takes one extra CC metric at most (two channels)",
        )
    extra = extras[0]
    if not isinstance(extra, (list, tuple)) or len(extra) != 5:
        raise ValueError(
            "fastwarp.ants.registration: multivariate_extras holds (metric, fixed, moving, "
            "weight, radius) tuples"
        )
    name, f2, m2, weight, r = extra
    if str(name).lower() != "cc":
        raise _unsupported(
            _REG, "multivariate_extras[0][0]", name, "FastWarp's SyN stage uses CC only"
        )
    if isinstance(r, bool) or not isinstance(r, (int, np.integer)) or r != radius:
        raise _unsupported(
            _REG,
            "multivariate_extras[0][4]",
            r,
            f"FastWarp's SyN stage uses one CC radius for both channels ({radius} here)",
        )
    for img, what in ((f2, "multivariate_extras[0][1]"), (m2, "multivariate_extras[0][2]")):
        if not ants.is_image(img):
            raise ValueError(f"fastwarp.ants.registration: {what} must be an ANTsImage")
        _check_image(img, what)
    fixed = a["fixed"]
    same_grid = tuple(f2.shape) == tuple(fixed.shape) and all(
        np.allclose(np.asarray(x), np.asarray(y), rtol=0.0, atol=1e-9)
        for x, y in (
            (f2.spacing, fixed.spacing),
            (f2.origin, fixed.origin),
            (f2.direction, fixed.direction),
        )
    )
    if not same_grid:
        raise _unsupported(
            _REG,
            "multivariate_extras[0][1]",
            f2,
            "it is not on fixed's grid, and FastWarp's SyN channels share one fixed grid; "
            "resample it onto that grid first",
        )
    try:
        ok = float(weight) >= 0.0
    except (TypeError, ValueError):
        ok = False
    if not ok:
        raise ValueError(
            f"fastwarp.ants.registration: multivariate_extras[0][3]={weight!r} must be a "
            "non-negative weight"
        )


def _linear_schedule(a: dict[str, Any]) -> tuple[Any, Any, Any]:
    """``(iterations, shrink factors, smoothing sigmas)`` of the linear types as ANTsPy builds
    them from ``aff_*``, with its checks; a list is taken like a tuple."""
    its = a["aff_iterations"]
    shrink_in = a["aff_shrink_factors"]
    sigmas_in = a["aff_smoothing_sigmas"]
    iterations: Any = its
    shrink: Any = "6x4x2x1"
    sigmas: Any = "3x2x1x0"
    if type(shrink_in) is int or type(sigmas_in) is int or type(its) is int:
        if type(sigmas_in) is not int:
            raise ValueError("aff_smoothing_sigmas should be a single integer.")
        if type(its) is not int:
            raise ValueError("aff_iterations should be a single integer.")
        if type(shrink_in) is not int:
            raise ValueError("aff_shrink_factors should be a single integer.")
        return its, shrink_in, sigmas_in
    if isinstance(shrink_in, (tuple, list)):
        shrink = _join(shrink_in)
        sigmas = _join(sigmas_in)
        iterations = _join(its)
    if type(shrink_in) is tuple:
        if len(its) != len(sigmas_in):
            raise ValueError("aff_iterations length should equal aff_smoothing_sigmas length.")
        if len(its) != len(shrink_in):
            raise ValueError("aff_iterations length should equal aff_shrink_factors length.")
        if len(shrink_in) != len(sigmas_in):
            raise ValueError("aff_shrink_factors length should equal aff_smoothing_sigmas length.")
    return iterations, shrink, sigmas


def _registration_argv(t: str, a: dict[str, Any], names: dict[str, str]) -> list[str]:
    """The ``antsRegistration`` argument list ANTsPy 0.6.3 builds for a type FastWarp runs, with
    ``names`` for its image pointers (``fixed``, ``moving``, ``warpedmovout``,
    ``warpedfixout``, ``mask``, ``extra_fixed``, ``extra_moving``) and ``a`` the arguments."""
    f = names["fixed"]
    m = names["moving"]
    fmask = names["mask"] if a["mask"] is not None else "NA"
    maskopt = f"[{fmask},NA]"
    earlymaskopt = maskopt if a["mask_all_stages"] else "[NA,NA]"
    initial = a["initial_transform"]
    if isinstance(initial, str):
        initial = [initial]
    if initial is None:
        initial = ["Identity"] if t in _DEFORMABLE_ONLY else [f"[{f},{m},1]"]
    outprefix = a["outprefix"]
    output = f"[{outprefix},{names['warpedmovout']},{names['warpedfixout']}]"
    hm = str(int(a["use_legacy_histogram_matching"]))
    am = a["aff_metric"]
    ar = a["aff_random_sampling_rate"]
    aff = f"{am}[{f},{m},1,{a['aff_sampling']},regular,{ar}]"
    syn = f"{a['syn_metric']}[{f},{m},1,{a['syn_sampling']}]"
    mysyn = "SyN[{:f},{:f},{:f}]".format(a["grad_step"], a["flow_sigma"], a["total_sigma"])
    synits, synsig, synshr = ("", "", "")
    if t in _MYSYN:
        synits, synsig, synshr = _syn_schedule(a["reg_iterations"])
    head = ["--dimensionality", "3", "-r", *initial]
    tail = ["-u", hm, "-z", "1", "--output", output]

    def stage(metric: str, transform: str, conv: str, sig: str, shr: str) -> list[str]:
        return [
            "--metric",
            metric,
            "--transform",
            transform,
            "--convergence",
            conv,
            "--smoothing-sigmas",
            sig,
            "--shrink-factors",
            shr,
        ]

    syn_stage = stage(syn, mysyn, f"[{synits},1e-7,8]", synsig, synshr)
    early = ["-x", earlymaskopt]
    args: list[str]
    if t == "SyNBold":
        args = [
            *head,
            *stage(aff, "Rigid[0.25]", "[1200x1200x100,1e-6,5]", "2x1x0", "4x2x1"),
            *early,
            *syn_stage,
            *tail,
            "-x",
            maskopt,
        ]
    elif t == "SyNBoldAff":
        args = [
            *head,
            *stage(aff, "Rigid[0.25]", "[1200x1200x100,1e-6,5]", "2x1x0", "4x2x1"),
            *early,
            *stage(aff, "Affine[0.25]", "[200x20,1e-6,5]", "1x0", "2x1"),
            *early,
            *syn_stage,
            *tail,
            "-x",
            maskopt,
        ]
    elif t == "ElasticSyN":
        args = [
            *head,
            *stage(aff, "Affine[0.25]", "2100x1200x200x0", "3x2x1x0", "4x2x2x1"),
            *early,
            *syn_stage,
            *tail,
            "-x",
            maskopt,
        ]
    elif t == "SyN":
        args = [
            *head,
            *stage(aff, "Affine[0.25]", "2100x1200x1200x0", "3x2x1x0", "4x2x2x1"),
            *early,
            *syn_stage,
            *tail,
            "-x",
            maskopt,
        ]
    elif t == "SyNRA":
        args = [
            *head,
            *stage(aff, "Rigid[0.25]", "2100x1200x1200x0", "3x2x1x0", "4x2x2x1"),
            *early,
            *stage(aff, "Affine[0.25]", "2100x1200x1200x0", "3x2x1x0", "4x2x2x1"),
            *early,
            *syn_stage,
            *tail,
            "-x",
            maskopt,
        ]
    elif t == "SyNOnly":
        metrics = ["--metric", syn]
        if a["multivariate_extras"] is not None:
            for name, _f2, _m2, weight, radius in a["multivariate_extras"]:
                metrics += [
                    "--metric",
                    f"{name}[{names['extra_fixed']},{names['extra_moving']},{weight},{radius}]",
                ]
        args = [
            *head,
            *metrics,
            "--transform",
            mysyn,
            "--convergence",
            f"[{synits},1e-7,8]",
            "--smoothing-sigmas",
            synsig,
            "--shrink-factors",
            synshr,
            *tail,
            "-x",
            maskopt,
        ]
    elif t in ("SyNAggro", "SyNLessAggro"):
        args = [
            *head,
            *stage(aff, "Affine[0.25]", "2100x1200x1200x100", "3x2x1x0", "4x2x2x1"),
            *early,
            *syn_stage,
            *tail,
            "-x",
            maskopt,
        ]
    elif t == "SyNCC":
        args = [
            *head,
            *stage(aff, "Rigid[1]", "2100x1200x1200x0", "3x2x1x0", "4x4x2x1"),
            *early,
            *stage(aff, "Affine[1]", "1200x1200x100", "2x1x0", "4x2x1"),
            *early,
            *stage(
                f"CC[{f},{m},1,4]",
                "SyN[0.15,3,0]",
                "[2100x1200x1200x20,1e-7,8]",
                "3x2x1x0",
                "4x3x2x1",
            ),
            *tail,
            "-x",
            maskopt,
        ]
    elif t == "SyNabp":
        mattes = f"mattes[{f},{m},1,32,regular,0.25]"
        args = [
            *head,
            *stage(mattes, "Rigid[0.1]", "1000x500x250x100", "4x2x1x0", "8x4x2x1"),
            *early,
            *stage(mattes, "Affine[0.1]", "1000x500x250x100", "4x2x1x0", "8x4x2x1"),
            *early,
            *stage(f"CC[{f},{m},0.5,4]", "SyN[0.1,3,0]", "50x10x0", "2x1x0", "4x2x1"),
            *tail,
            "-x",
            maskopt,
        ]
    elif t in _LINEAR:
        iterations, shrink, sigmas = _linear_schedule(a)
        kind = "Affine" if t in ("Affine", "AffineFast", "BOLDAffine") else "Rigid"
        if t == "AffineFast":
            iterations = "2100x1200x0x0"
        elif t in ("BOLDAffine", "BOLDRigid"):
            shrink, sigmas, iterations = "2x1", "1x0", "100x20"
        elif t == "QuickRigid":
            iterations = "20x20x0x0"
        elif t == "DenseRigid":
            ar = 1.0
        if a["smoothing_in_mm"]:
            sigmas = f"{sigmas}mm"
        args = [
            *head,
            *stage(
                f"{am}[{f},{m},1,{a['aff_sampling']},regular,{ar}]",
                f"{kind}[0.25]",
                str(iterations),
                str(sigmas),
                str(shrink),
            ),
            *tail,
            "-x",
            maskopt,
        ]
    else:
        args = _script_argv(t, a, names, head, output, maskopt)
    if a["random_seed"] is not None:
        args += ["--random-seed", str(a["random_seed"])]
    if a["restrict_transformation"] is not None:
        args += ["-g", _join(a["restrict_transformation"])]
    args += [
        "--float",
        str(int(a["singleprecision"])),
        "--write-composite-transform",
        str(a["write_composite_transform"] * 1),
    ]
    if a["verbose"]:
        args += ["-v", "1"]
    return args


def _script_argv(
    t: str, a: dict[str, Any], names: dict[str, str], head: list[str], output: str, maskopt: str
) -> list[str]:
    script = _script_type(t)
    sub = script["subtype"]
    f = names["fixed"]
    m = names["moving"]
    linear_metric = f"MI[{f},{m},1,32,Regular,0.25]"
    if script["quick"]:
        rigid_conv = affine_conv = "[1000x500x250x0,1e-6,10]"
    else:
        rigid_conv = affine_conv = "[1000x500x250x100,1e-6,10]"
    syn_conv = "[100x70x50x20,1e-6,10]"
    syn_shrink, syn_sigmas = "8x4x2x1", "3x2x1x0vox"
    if sub == "sr":
        syn_conv = "[50x20,1e-6,10]"
        syn_shrink, syn_sigmas = "2x1", "1x0vox"
    rigid = [
        "--transform",
        "Rigid[0.1]",
        "--metric",
        linear_metric,
        "--convergence",
        rigid_conv,
        "--shrink-factors",
        "8x4x2x1",
        "--smoothing-sigmas",
        "3x2x1x0vox",
    ]
    affine = [
        "--transform",
        "Affine[0.1]",
        "--metric",
        linear_metric,
        "--convergence",
        affine_conv,
        "--shrink-factors",
        "8x4x2x1",
        "--smoothing-sigmas",
        "3x2x1x0vox",
    ]
    syn = ["--transform", "SyN[0.2,3,0]", "--metric", f"CC[{f},{m},1,2]"]
    if a["multivariate_extras"] is not None:
        for name, _f2, _m2, weight, radius in a["multivariate_extras"]:
            syn += [
                "--metric",
                f"{name}[{names['extra_fixed']},{names['extra_moving']},{weight},{radius}]",
            ]
    syn += [
        "--convergence",
        syn_conv,
        "--shrink-factors",
        syn_shrink,
        "--smoothing-sigmas",
        syn_sigmas,
    ]
    stages = {
        "r": rigid,
        "a": rigid + affine,
        "s": rigid + affine + syn,
        "sr": rigid + syn,
        "so": syn,
    }[sub]
    return [*head, "--output", output, *stages, "-x", maskopt]


def _uses_extras(t: str, script: dict[str, Any] | None) -> bool:
    return t == "SyNOnly" or (script is not None and script["subtype"] in ("s", "sr", "so"))


def _last_stage_is_syn(t: str, script: dict[str, Any] | None) -> bool:
    if script is not None:
        return script["subtype"] in ("s", "sr", "so")
    return t not in _LINEAR


def _validate(ants: Any, a: dict[str, Any]) -> tuple[str, dict[str, Any] | None]:
    """ANTsPy's own checks in ANTsPy's order, then FastWarp's; returns the type of transform
    as ANTsPy normalizes it and the parts of an ``antsRegistrationSyN[...]`` type."""
    fixed, moving = a["fixed"], a["moving"]
    if not (ants.is_image(fixed) and ants.is_image(moving)):
        raise ValueError("Fixed and moving images must be ANTsImage objects")
    t = a["type_of_transform"]
    if t == "":
        t = "SyN"
    if isinstance(t, (tuple, list)) and len(t) == 1:
        t = t[0]
    if np.sum(np.isnan(fixed.numpy())) > 0:
        raise ValueError("fixed image has NaNs - replace these")
    if np.sum(np.isnan(moving.numpy())) > 0:
        raise ValueError("moving image has NaNs - replace these")
    if fixed.dimension != moving.dimension:
        raise ValueError("Fixed and moving image dimensions are not the same.")
    _linear_schedule(a)
    if not isinstance(t, str) or ("antsRegistrationSyN" not in t and t not in _ALLOWABLE):
        raise ValueError(f"{t} does not exist")
    registration_module = sys.modules.get("ants.registration.registration")
    if getattr(registration_module, "_deterministic", False) and "Repro" not in t:
        raise ValueError(f"{t} is not deterministic/reproducible.")
    _check_image(fixed, "fixed")
    _check_image(moving, "moving")
    script = _check_type(t)
    if t in _MYSYN:
        _syn_metric_check(a)
        n = len(a["reg_iterations"])
        if n == 0 or n > 10:
            raise _unsupported(
                _REG,
                "reg_iterations",
                a["reg_iterations"],
                "ANTsPy derives the smoothing sigmas of 1 to 10 levels only",
            )
    if t in _AFF_METRIC:
        _aff_metric_check(a, t)
    initial = a["initial_transform"]
    if initial is not None:
        items = [initial] if isinstance(initial, str) else initial
        if not isinstance(items, (list, tuple)) or not all(isinstance(i, str) for i in items):
            raise TypeError(
                "fastwarp.ants.registration: initial_transform must be None, 'Identity', a file "
                "name or a list of one file name"
            )
        if len(items) != 1:
            raise _unsupported(
                _REG,
                "initial_transform",
                initial,
                "FastWarp starts the registration from one affine transform file",
            )
        parts = items[0].strip("[] ").split(",")
        if len(parts) == 3:
            raise _unsupported(
                _REG,
                "initial_transform",
                initial,
                "the images are ANTsImages here, not files; leave initial_transform=None for "
                "the centres of intensity mass",
            )
        name = parts[0].strip().lower()
        if name.endswith((".nii", ".nii.gz")):
            raise _unsupported(
                _REG,
                "initial_transform",
                initial,
                "a displacement field cannot start the registration; FastWarp starts from one "
                "affine transform file",
            )
    if a["mask"] is not None:
        if not ants.is_image(a["mask"]):
            raise ValueError("fastwarp.ants.registration: mask must be an ANTsImage")
        _check_image(a["mask"], "mask")
        if _last_stage_is_syn(t, script):
            raise _unsupported(
                _REG,
                "mask",
                a["mask"],
                f"with type_of_transform={t!r} ANTsPy applies the mask to the SyN stage, and "
                "FastWarp's SyN stage has no mask",
            )
    if a["moving_mask"] is not None:
        raise _unsupported(
            _REG, "moving_mask", a["moving_mask"], "FastWarp has no moving-image mask"
        )
    restrict = a["restrict_transformation"]
    if restrict is not None and not (
        isinstance(restrict, (tuple, list)) and all(_same(r, 1) for r in restrict)
    ):
        raise _unsupported(
            _REG,
            "restrict_transformation",
            restrict,
            "FastWarp cannot restrict transform components",
        )
    if a["use_legacy_histogram_matching"] and script is None:
        raise _unsupported(
            _REG,
            "use_legacy_histogram_matching",
            a["use_legacy_histogram_matching"],
            "FastWarp has no histogram matching",
        )
    if a["multivariate_extras"] is not None and _uses_extras(t, script):
        radius = a["syn_sampling"] if t == "SyNOnly" else 2
        _extras_check(a, ants, radius)
    seed = a["random_seed"]
    if seed is not None and not (isinstance(seed, (int, np.integer)) and 0 <= seed < 2**32):
        raise ValueError(
            f"fastwarp.ants.registration: ants.config._random_seed={seed!r} must be an integer "
            "from 0 to 4294967295"
        )
    return t, script


def _warn_ignored(t: str, script: dict[str, Any] | None, a: dict[str, Any]) -> None:
    """A ``UserWarning`` for each argument with a non-default value that ANTsPy ignores for
    ``t``."""
    why = f"with type_of_transform={t!r}"
    used: set[str] = {
        "fixed",
        "moving",
        "type_of_transform",
        "initial_transform",
        "outprefix",
        "mask",
        "moving_mask",
        "write_composite_transform",
        "verbose",
        "restrict_transformation",
        "singleprecision",
    }
    if script is None:
        used.add("use_legacy_histogram_matching")
    if t in _MYSYN:
        used |= {"grad_step", "flow_sigma", "total_sigma", "syn_metric", "syn_sampling"}
        used.add("reg_iterations")
    if t in _AFF_METRIC:
        used |= {"aff_metric", "aff_sampling", "aff_random_sampling_rate"}
        if t == "DenseRigid":
            used.discard("aff_random_sampling_rate")
    if t in ("Rigid", "Affine", "DenseRigid"):
        used |= {"aff_iterations", "aff_shrink_factors", "aff_smoothing_sigmas"}
    if t in ("AffineFast", "QuickRigid"):
        used |= {"aff_shrink_factors", "aff_smoothing_sigmas"}
    if t in _LINEAR:
        used.add("smoothing_in_mm")
    if t in _EARLY_MASKS:
        used.add("mask_all_stages")
    if _uses_extras(t, script):
        used.add("multivariate_extras")
    for name, default in _DEFAULTS.items():
        if name not in used and not _same(a[name], default):
            _ignored(_REG, name, a[name], why)
    if script is not None and script["radius_given"] is not None and not script["quick"]:
        warnings.warn(
            f"fastwarp.ants.registration: the CC radius {script['radius_given']} in "
            f"type_of_transform={t!r} has no effect: ANTsPy 0.6.3 uses radius 2 there (ANTsPy "
            "ignores it too).",
            UserWarning,
            stacklevel=3,
        )


def _raw_argv(args: Sequence[Any]) -> list[str]:
    """An argument list as ANTsPy passes it to ``antsRegistration`` (``process_arguments``):
    ``True`` / ``False`` as ``1`` / ``0``, ``None`` left out, the rest as text. Needs no ANTsPy:
    an ``ANTsImage`` in the list implies ANTsPy is imported already."""
    loaded = sys.modules.get("ants")
    out = []
    for arg in args:
        if loaded is not None and loaded.is_image(arg):
            raise _unsupported(
                _REG,
                "fixed",
                "[..., ANTsImage, ...]",
                "an argument list names images by their files; pass an ANTsImage as fixed and "
                "moving instead",
            )
        if arg is None:
            continue
        text = str(arg)
        out.append("1" if text == "True" else "0" if text == "False" else text)
    return out


def _antspy_args(given: dict[str, Any]) -> dict[str, Any]:
    """``given`` for ANTsPy itself, which takes the output prefix as text only."""
    return {**given, "outprefix": fspath(given["outprefix"])}


def _check_fallback(function: str, fallback: Any) -> None:
    if fallback not in (None, "ants"):
        raise ValueError(f"fastwarp.ants.{function}: fallback must be None or 'ants'")


def _fall_back(function: str, error: Exception) -> None:
    warnings.warn(
        f"fastwarp.ants.{function}: running ANTsPy instead of FastWarp (fallback='ants'): {error}",
        UserWarning,
        stacklevel=3,
    )


def _check_iso_mm(iso_mm: Any) -> float | None:
    if iso_mm is None:
        return None
    value = float(iso_mm)
    if not (math.isfinite(value) and value > 0.0):
        raise ValueError("fastwarp.ants.registration: iso_mm must be a positive number")
    return value


def _to_image(ants: Any, entry: tuple, pixeltype: str) -> Any:
    data, size, spacing, origin, direction = entry
    arr = np.asarray(data).reshape(tuple(size), order="F")
    img = ants.from_numpy(
        arr, origin=list(origin), spacing=list(spacing), direction=np.asarray(direction)
    )
    return img.clone(pixeltype)


def _relabel(e: Exception) -> Exception:
    return type(e)(_readable(str(e)))


def registration(
    fixed: Any,
    moving: Any,
    type_of_transform: str = "SyN",
    initial_transform: str | list[str] | None = None,
    outprefix: str | PathLike[str] = "",
    mask: Any = None,
    moving_mask: Any = None,
    mask_all_stages: bool = False,
    grad_step: float = 0.2,
    flow_sigma: float = 3,
    total_sigma: float = 0,
    aff_metric: str = "mattes",
    aff_sampling: int = 32,
    aff_random_sampling_rate: float = 0.2,
    syn_metric: str = "mattes",
    syn_sampling: int = 32,
    reg_iterations: Sequence[int] = (40, 20, 0),
    aff_iterations: Sequence[int] | int = (2100, 1200, 1200, 10),
    aff_shrink_factors: Sequence[int] | int = (6, 4, 2, 1),
    aff_smoothing_sigmas: Sequence[float] | int = (3, 2, 1, 0),
    write_composite_transform: bool = False,
    verbose: bool = False,
    multivariate_extras: Sequence[tuple] | None = None,
    restrict_transformation: Sequence[float] | None = None,
    smoothing_in_mm: bool = False,
    singleprecision: bool = True,
    use_legacy_histogram_matching: bool = False,
    *,
    iso_mm: float | None = None,
    adapter: str | None = None,
    cancel: CancelToken | None = None,
    fallback: str | None = None,
) -> dict[str, Any] | int:
    """Register ``moving`` onto ``fixed`` on the GPU, as ``ants.registration`` does.

    Takes the arguments of ANTsPy's ``ants.registration`` (ANTsPy 0.6.3), builds the
    ``antsRegistration`` argument list ANTsPy builds for them, and runs it with FastWarp's ANTs
    compatibility mode, the same code as ``fastwarp ants-registration``. The transform files
    have ANTs' names and the result has ANTsPy's form. See :doc:`/guide/ants` for the types of
    transform FastWarp runs and the differences to ANTs.

    Parameters
    ----------
    fixed : ANTsImage or list of str
        The image to register onto, 3-D and scalar. A list of ``antsRegistration`` arguments
        (with ``moving=None``) runs them as ``fastwarp ants-registration`` does and returns 0, as
        ANTsPy does; this form needs no ANTsPy.
    moving : ANTsImage or None
        The image to register.
    type_of_transform : str, default "SyN"
        The stages, by ANTsPy's name. FastWarp runs ``Rigid``, ``Affine``, ``AffineFast``,
        ``BOLDAffine``, ``QuickRigid``, ``DenseRigid``, ``BOLDRigid``, ``SyNCC``, ``SyNabp``,
        ``antsRegistrationSyN[r|a|s|sr|so]``, ``antsRegistrationSyNQuick[r|a]``, and ``SyN``,
        ``SyNRA``, ``SyNOnly``, ``SyNAggro``, ``SyNLessAggro``, ``ElasticSyN``, ``SyNBold`` and
        ``SyNBoldAff`` with ``syn_metric="CC"``. The others raise
        :class:`UnsupportedArgument`. The default ``"SyN"`` uses Mattes mutual information in its
        SyN stage, which FastWarp does not have: pass ``"SyNCC"``, or ``syn_metric="CC"``.
    initial_transform : str or list of str, optional
        ``"Identity"``, or one file holding one affine transform (``.mat``, ``.txt``, or an
        ``.h5`` such as the ``Composite.h5`` of a linear run), optionally as ``"[file,1]"`` for
        its inverse. By default the centres of intensity mass, or the identity
        for ``SyNOnly`` and ``antsRegistrationSyN[so]``.
    outprefix : str or os.PathLike, default ""
        Prefix of the transform files, joined to their names as text. Its folder is created
        when missing. ``""`` makes a new private temporary folder (``tempfile.mkdtemp``) with
        the prefix ``fw_`` in it, where ANTsPy uses ``tempfile.mktemp``.
    mask : ANTsImage, optional
        Fixed-image mask (non-zero voxels inside), for types whose last stage is linear.
    moving_mask : None
        Not supported.
    mask_all_stages : bool, default False
        As in ANTsPy; with a mask only the linear types run, so it changes nothing.
    grad_step, flow_sigma, total_sigma : float
        The SyN stage's gradient step, update-field and total-field variance (voxels²), for the
        types that take them.
    aff_metric : str, default "mattes"
        ``"mattes"`` (or ``"MI"``); ``"GC"`` and ``"meansquares"`` are not supported.
    aff_sampling : int, default 32
        Mattes histogram bins; 32 only.
    aff_random_sampling_rate : float, default 0.2
        Fraction of the voxels the linear metric samples, on a regular grid.
    syn_metric : str, default "mattes"
        ``"CC"`` only (``"mattes"``, the default, is refused).
    syn_sampling : int, default 32
        With ``syn_metric="CC"``, the CC radius, 1 to 8.
    reg_iterations : sequence of int, default (40, 20, 0)
        SyN iterations per level (1 to 10 levels); the shrink factors and sigmas follow from the
        number of levels as in ANTsPy.
    aff_iterations, aff_shrink_factors, aff_smoothing_sigmas : sequence of int or int
        The schedule of the linear types. A list is taken like a tuple (ANTsPy builds a broken
        argument from a list).
    write_composite_transform : bool, default False
        Write ``Composite.h5`` and ``InverseComposite.h5`` only.
    verbose : bool, default False
        Print the ``antsRegistration`` arguments and FastWarp's report of the run.
    multivariate_extras : list of tuple, optional
        One ``("CC", fixed2, moving2, weight, radius)`` with the main CC radius, a second
        channel, for ``SyNOnly`` and ``antsRegistrationSyN[s|sr|so]``. ``fixed2`` must be on
        ``fixed``'s grid.
    restrict_transformation : sequence of float, optional
        All ones only (no restriction).
    smoothing_in_mm : bool, default False
        Sigmas of the linear types in millimetres.
    singleprecision : bool, default True
        Field files in ``float32`` (``True``) or ``float64``; FastWarp computes in ``float32``
        on the GPU either way.
    use_legacy_histogram_matching : bool, default False
        ``True`` is not supported.
    iso_mm : float, optional
        Keyword only, FastWarp only. Voxel size of the registration grid in millimetres; by
        default the fixed image's smallest spacing.
    adapter : str, optional
        Keyword only, FastWarp only. Use the GPU whose name contains this text.
    cancel : CancelToken, optional
        Keyword only, FastWarp only. A token that another thread can use to stop the run.
    fallback : {None, "ants"}, optional
        Keyword only, FastWarp only. With ``"ants"``, an argument FastWarp refuses, no usable
        GPU, or a registration that does not fit the GPU runs ANTsPy's ``ants.registration``
        with the same arguments instead, with a ``UserWarning``; the result then holds
        ``"engine"``.

    Returns
    -------
    dict or int
        ``warpedmovout`` (``moving`` on ``fixed``'s grid) and ``warpedfixout`` (``fixed`` on
        ``moving``'s grid), both ``ANTsImage`` of ``fixed``'s pixel type, linear interpolation;
        ``fwdtransforms`` and ``invtransforms``, lists of the transform files in the order
        ``apply_transforms`` takes them, or the composite file names with
        ``write_composite_transform``. With ``fallback="ants"`` also ``engine``: ``"fastwarp"``
        or ``"ants"``. An argument list returns 0.

    Raises
    ------
    UnsupportedArgument
        An argument FastWarp does not support. Raised before any work.
    ValueError
        An invalid argument, with ANTsPy's message where ANTsPy checks it.
    TypeError
        An unknown keyword argument (ANTsPy ignores those).
    ImportError
        ANTsPy is not installed.
    GpuUnavailableError
        No usable GPU. Raised before any work.
    GpuCapacityError
        The registration does not fit the GPU's memory. Raised before any work.
    RegistrationCancelled
        ``cancel`` was cancelled.
    RegistrationError
        The registration failed, or a file could not be read or written.

    See Also
    --------
    apply_transforms : Apply the transforms this function writes.
    fastwarp.register_syn : FastWarp's own registration call.
    """
    _check_fallback(_REG, fallback)
    given = {
        "type_of_transform": type_of_transform,
        "initial_transform": initial_transform,
        "outprefix": outprefix,
        "mask": mask,
        "moving_mask": moving_mask,
        "mask_all_stages": mask_all_stages,
        "grad_step": grad_step,
        "flow_sigma": flow_sigma,
        "total_sigma": total_sigma,
        "aff_metric": aff_metric,
        "aff_sampling": aff_sampling,
        "aff_random_sampling_rate": aff_random_sampling_rate,
        "syn_metric": syn_metric,
        "syn_sampling": syn_sampling,
        "reg_iterations": reg_iterations,
        "aff_iterations": aff_iterations,
        "aff_shrink_factors": aff_shrink_factors,
        "aff_smoothing_sigmas": aff_smoothing_sigmas,
        "write_composite_transform": write_composite_transform,
        "verbose": verbose,
        "multivariate_extras": multivariate_extras,
        "restrict_transformation": restrict_transformation,
        "smoothing_in_mm": smoothing_in_mm,
        "singleprecision": singleprecision,
        "use_legacy_histogram_matching": use_legacy_histogram_matching,
    }
    iso = _check_iso_mm(iso_mm)
    if isinstance(fixed, list) and moving is None:
        for name, default in _DEFAULTS.items():
            if not _same(given[name], default):
                _ignored(_REG, name, given[name], "with an argument list", 3)
        try:
            argv = _raw_argv(fixed)
            out = _core.ants_registration(argv, iso_mm=iso, adapter=adapter, cancel=cancel)
        except (UnsupportedArgument, GpuUnavailableError, GpuCapacityError) as e:
            if fallback != "ants":
                raise
            _fall_back(_REG, e)
            return _ants().registration(fixed, None)
        if out["kind"] != "run":
            print(out["text"], end="")
            return 0
        for w in out["warnings"]:
            warnings.warn(f"fastwarp.ants.registration: {w}", UserWarning, stacklevel=2)
        if out["plan"]["verbose"]:
            print(out["report"], end="")
        return 0

    ants = _ants()
    a = {"fixed": fixed, "moving": moving, **given}
    a["random_seed"] = getattr(sys.modules.get("ants.config"), "_random_seed", None)
    try:
        t, script = _validate(ants, a)
    except UnsupportedArgument as e:
        if fallback != "ants":
            raise
        _fall_back(_REG, e)
        return {**ants.registration(fixed, moving, **_antspy_args(given)), "engine": "ants"}
    _warn_ignored(t, script, a)

    made = None
    if fspath(outprefix) == "":
        made = tempfile.mkdtemp(prefix="fastwarp_")
        prefix = os.path.join(made, "fw_")
    else:
        prefix = _prepared_prefix(outprefix)
    a["outprefix"] = prefix
    names = _registration_names()
    argv = _registration_argv(t, a, names)
    images = [(_FIXED, _image_args(fixed)), (_MOVING, _image_args(moving))]
    if mask is not None:
        binary = (mask.numpy() != 0).astype(np.float32)
        images.append((_MASK, _image_args((binary, mask.spacing, mask.origin, mask.direction))))
    if multivariate_extras is not None and _uses_extras(t, script):
        _, f2, m2, _, _ = multivariate_extras[0]
        images += [
            (names["extra_fixed"], _image_args(f2)),
            (names["extra_moving"], _image_args(m2)),
        ]
    if verbose:
        print("antsRegistration " + _readable(" ".join(argv)))
    try:
        out = _core.ants_registration(
            argv,
            images=images,
            keep=[_WARPED_MOVING, _WARPED_FIXED],
            iso_mm=iso,
            adapter=adapter,
            cancel=cancel,
        )
    except BaseException as e:
        if made is not None:
            _remove_empty(made)
        refused = (UnsupportedArgument, GpuUnavailableError, GpuCapacityError)
        if isinstance(e, refused) and fallback == "ants":
            _fall_back(_REG, _relabel(e))
            return {**ants.registration(fixed, moving, **_antspy_args(given)), "engine": "ants"}
        if isinstance(e, (ValueError, RuntimeError)):
            raise _relabel(e) from None
        raise
    for w in out["warnings"]:
        warnings.warn(f"fastwarp.ants.registration: {w}", UserWarning, stacklevel=2)
    if verbose:
        print(_readable(out["report"]), end="")
    pixeltype = fixed.pixeltype
    result: dict[str, Any] = {
        "warpedmovout": _to_image(ants, out["kept"][_WARPED_MOVING], pixeltype),
        "warpedfixout": _to_image(ants, out["kept"][_WARPED_FIXED], pixeltype),
    }
    if write_composite_transform:
        result["fwdtransforms"] = prefix + "Composite.h5"
        result["invtransforms"] = prefix + "InverseComposite.h5"
    else:
        rust_prefix = out["plan"]["prefix"]
        files = [prefix + f[len(rust_prefix) :] for f in out["transform_files"]]
        affine = [f for f in files if f.endswith("GenericAffine.mat")]
        warp = [
            f for f in files if f.endswith("Warp.nii.gz") and not f.endswith("InverseWarp.nii.gz")
        ]
        inverse = [f for f in files if f.endswith("InverseWarp.nii.gz")]
        result["fwdtransforms"] = warp + affine
        result["invtransforms"] = affine + inverse
    if fallback == "ants":
        result["engine"] = "fastwarp"
    return result


def _remove_empty(folder: str) -> None:
    with contextlib.suppress(OSError):
        os.rmdir(folder)


def _apply_argv(
    transformlist: list[str],
    interpolator: str,
    whichtoinvert: Sequence[bool],
    compose: str | None,
    imagetype: Any,
    defaultvalue: Any,
    singleprecision: bool,
    verbose: bool,
    names: dict[str, str],
) -> tuple[list[str], str]:
    """The ``antsApplyTransforms`` argument list ANTsPy 0.6.3 builds and the composed file's
    name (``"NA"`` without ``compose``)."""
    mytx: list[str] = []
    for i, path in enumerate(transformlist):
        mytx += ["-t", f"[{path},1]" if whichtoinvert[i] else path]
    tfn = "NA"
    if compose:
        tfn = compose if compose.endswith(".h5") else f"{compose}comptx.nii.gz"
    output = names["warped"] if compose is None else f"[{tfn},1]"
    args = ["-d", "3", "-i", names["moving"], "-o", output, "-r", names["fixed"]]
    args += ["-n", interpolator, *mytx]
    args += ["-z", "1", "-v", str(int(verbose)), "--float", str(int(singleprecision))]
    args += ["-e", str(imagetype), "-f", str(defaultvalue)]
    return args, tfn


def _which_to_invert(transformlist: list[str], whichtoinvert: Any) -> tuple[bool, ...]:
    """ANTsPy's ``whichtoinvert`` with its default and its checks."""
    if whichtoinvert is None or (
        isinstance(whichtoinvert, (tuple, list)) and sum(w is not None for w in whichtoinvert) == 0
    ):
        if (
            len(transformlist) == 2
            and ".mat" in transformlist[0]
            and ".mat" not in transformlist[1]
        ):
            whichtoinvert = (True, False)
        else:
            whichtoinvert = tuple([False] * len(transformlist))
    if len(whichtoinvert) != len(transformlist):
        raise ValueError("Transform list and inversion list must be the same length")
    for i, path in enumerate(transformlist):
        if whichtoinvert[i] and ".mat" not in path:
            raise ValueError(f"Cannot invert transform {i} ({path}) because it is not a matrix")
    return tuple(bool(w) for w in whichtoinvert)


def apply_transforms(
    fixed: Any,
    moving: Any,
    transformlist: str | list[str],
    interpolator: str = "linear",
    imagetype: int = 0,
    whichtoinvert: Sequence[bool] | None = None,
    compose: str | None = None,
    defaultvalue: float = 0,
    singleprecision: bool = False,
    verbose: bool = False,
    *,
    fallback: str | None = None,
) -> Any:
    """Resample ``moving`` onto ``fixed``'s grid through transform files, as
    ``ants.apply_transforms`` does.

    Takes the arguments of ANTsPy's ``ants.apply_transforms`` (ANTsPy 0.6.3), builds the
    ``antsApplyTransforms`` argument list ANTsPy builds, and runs it on the CPU with FastWarp's
    ANTs compatibility mode, the same code as ``fastwarp ants-apply-transforms``.

    Parameters
    ----------
    fixed : ANTsImage
        The output grid and pixel type, 3-D.
    moving : ANTsImage
        The image to resample, 3-D and scalar.
    transformlist : str or list of str
        Transform files (``.mat``, ``.txt``, ``.h5``, ``.nii.gz``); a point of ``fixed`` goes
        through the first file first.
    interpolator : str, default "linear"
        ``"linear"``, ``"nearestNeighbor"`` or ``"lanczosWindowedSinc"``. ANTsPy's other names
        raise :class:`UnsupportedArgument`.
    imagetype : int, default 0
        0 (scalar) only.
    whichtoinvert : sequence of bool, optional
        One flag per transform; only ``.mat`` files can be inverted. By default, as in ANTsPy,
        ``(True, False)`` for exactly two files of which only the first is a ``.mat`` file, and
        nothing inverted otherwise.
    compose : str, optional
        Write the transforms as one displacement field instead: ``compose + "comptx.nii.gz"``,
        or ``compose`` itself when it ends in ``.h5``. Returns that file name.
    defaultvalue : float, default 0
        Value of voxels that map outside ``moving``.
    singleprecision : bool, default False
        A composed field in ``float32`` instead of ``float64``. Resampled images are computed in
        ``float32`` either way (ANTsPy computes in ``float64`` unless this is ``True``), then
        cast to ``fixed``'s pixel type.
    verbose : bool, default False
        Print the ``antsApplyTransforms`` arguments.
    fallback : {None, "ants"}, optional
        Keyword only, FastWarp only. With ``"ants"``, an argument FastWarp refuses runs ANTsPy's
        ``ants.apply_transforms`` with the same arguments instead, with a ``UserWarning``.

    Returns
    -------
    ANTsImage or str or None
        The resampled image on ``fixed``'s grid with ``fixed``'s pixel type; with ``compose``,
        the composed file's name, or ``None`` when it was not written.

    Raises
    ------
    UnsupportedArgument
        An argument FastWarp does not support. Raised before any work.
    ValueError
        An invalid argument, with ANTsPy's message where ANTsPy checks it.
    FileNotFoundError
        A transform file does not exist.
    ImportError
        ANTsPy is not installed.
    RegistrationError
        A transform file cannot be read, or the output cannot be written.
    """
    _check_fallback(_APPLY, fallback)
    ants = _ants()
    if not isinstance(transformlist, (tuple, list)) and transformlist is not None:
        transformlist = [transformlist]
    given = {
        "interpolator": interpolator,
        "imagetype": imagetype,
        "whichtoinvert": whichtoinvert,
        "compose": compose,
        "defaultvalue": defaultvalue,
        "singleprecision": singleprecision,
        "verbose": verbose,
    }
    try:
        if interpolator not in _ACCEPTED_INTERPOLATORS:
            raise ValueError(f"interpolator not supported - see {_ACCEPTED_INTERPOLATORS}")
        if isinstance(fixed, str):
            raise _unsupported(
                _APPLY,
                "fixed",
                fixed,
                "pass an ANTsImage (the argument-list form of ants.apply_transforms is not "
                "supported)",
            )
        if not (ants.is_image(fixed) and ants.is_image(moving)):
            raise ValueError("fixed and moving must be ANTsImage objects")
        paths = [fspath(p) for p in transformlist]
        for path in paths:
            if not os.path.exists(path):
                raise FileNotFoundError(f"Transform {path} does not exist")
        if fixed.dimension != 3:
            raise _unsupported(
                _APPLY, "fixed.dimension", fixed.dimension, "FastWarp handles 3-D images only"
            )
        _check_image(moving, "moving", _APPLY)
        if not _same(imagetype, 0):
            raise _unsupported(
                _APPLY, "imagetype", imagetype, "FastWarp resamples scalar 3-D images only"
            )
        if interpolator not in _SUPPORTED_INTERPOLATORS:
            raise _unsupported(
                _APPLY,
                "interpolator",
                interpolator,
                "FastWarp interpolates with linear, nearestNeighbor or lanczosWindowedSinc",
            )
        invert = _which_to_invert(paths, whichtoinvert)
        if compose == "":
            raise ValueError("fastwarp.ants.apply_transforms: compose='' names no file")
    except UnsupportedArgument as e:
        if fallback != "ants":
            raise
        _fall_back(_APPLY, e)
        return ants.apply_transforms(fixed, moving, [fspath(t) for t in transformlist], **given)
    names = {"fixed": _FIXED, "moving": _MOVING, "warped": _WARPED_MOVING}
    argv, tfn = _apply_argv(
        paths,
        interpolator,
        invert,
        compose,
        imagetype,
        defaultvalue,
        singleprecision,
        verbose,
        names,
    )
    if verbose:
        print([_readable(a) for a in argv])
    try:
        out = _core.ants_apply_transforms(
            argv,
            images=[] if compose is not None else [(_MOVING, _image_args(moving))],
            grids=[(_FIXED, _grid_args(fixed))],
            keep=[_WARPED_MOVING],
        )
    except UnsupportedArgument as e:
        if fallback != "ants":
            raise _relabel(e) from None
        _fall_back(_APPLY, _relabel(e))
        return ants.apply_transforms(fixed, moving, [fspath(t) for t in transformlist], **given)
    except (ValueError, RuntimeError) as e:
        raise _relabel(e) from None
    if compose is not None:
        return tfn if os.path.exists(tfn) else None
    return _to_image(ants, out["kept"][_WARPED_MOVING], fixed.pixeltype)
