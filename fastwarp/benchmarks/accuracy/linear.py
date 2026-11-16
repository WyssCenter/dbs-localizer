# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Linear fits: FastWarp's ``linear_session`` against ``antsRegistration`` running the same
stage, on the images the registration runs use.

Per pair, each engine fits a rigid transform from the centres of intensity mass and then an
affine transform from its own rigid result, with the recipe's ``rigid`` and ``affine`` stages.
FastWarp also runs a multi-start (:data:`MULTISTART`) in the same session; ANTs has no
equivalent option, so that row has no ANTs time. The accuracy figures are the landmark error of
each fit (the fixed landmarks moved by the affine, against the moving landmarks) where the
dataset has landmarks, and the distance between the two engines' transforms over the fixed
brain mask.

Layout under the work folder::

    <dataset>/linear/<fixed>__<moving>/{fastwarp,ants}/run.json   times, transforms
    <dataset>/linear/<fixed>__<moving>/comparison.json            landmark errors, agreement
"""

from __future__ import annotations

import json
import math
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import numpy as np

from . import datasets, landmarks, metrics, recipes, timing
from .run import (
    ENGINES,
    RunLock,
    _now,
    _read_json,
    _record,
    _write_json,
    ants_program,
    child_env,
    environment,
    itk_threads,
    pair_id,
    prepare_fixed,
    prepare_moving,
    rayon_threads,
    run_child,
    same_config,
    select_pairs,
    thread_env,
)

FOLDER = "linear"
#: The datasets ``linear`` runs without ``--dataset``: the default-run datasets with landmarks.
DEFAULT_KEYS = ("snsx",)
#: The fits of a pair, in the order they run.
FITS = ("rigid", "affine", "multistart")
#: FastWarp's multi-start: a coarse rigid fit (the first two levels of the rigid stage) from the
#: centre-of-mass start rotated by each of ``rotations_deg`` (degrees about the x, y and z axes
#: of the fixed image's physical space, through the centre), then the ``refine`` best of them
#: refined with the full rigid stage; the result is the refined fit with the lowest MI.
MULTISTART: dict[str, Any] = {
    "rotations_deg": [[0, 0, 0], [15, 0, 0], [-15, 0, 0], [0, 0, 15], [0, 0, -15]],
    "coarse_levels": 2,
    "refine": 2,
}


def config(rec: dict[str, dict[str, Any]]) -> dict[str, Any]:
    """What a linear run depends on: the recipe's rigid and affine stages, the winsorizing
    quantiles and the multi-start."""
    return {
        "rigid": rec["rigid"],
        "affine": rec["affine"],
        "winsorize": list(recipes.WINSORIZE),
        "multistart": MULTISTART,
    }


def coarse_stage(stage: dict[str, Any], levels: int) -> dict[str, Any]:
    """``stage`` with only its first ``levels`` levels."""
    out = dict(stage)
    for k in ("iterations", "shrink_factors", "smoothing_sigmas"):
        out[k] = list(stage[k][:levels])
    return out


def rotation_matrix(angles_deg: Any) -> np.ndarray:
    """``Rz @ Ry @ Rx`` for rotations by ``angles_deg`` (x, y, z) in degrees."""
    ax, ay, az = np.deg2rad(np.asarray(angles_deg, dtype=np.float64))
    rx = np.array([[1, 0, 0], [0, np.cos(ax), -np.sin(ax)], [0, np.sin(ax), np.cos(ax)]])
    ry = np.array([[np.cos(ay), 0, np.sin(ay)], [0, 1, 0], [-np.sin(ay), 0, np.cos(ay)]])
    rz = np.array([[np.cos(az), -np.sin(az), 0], [np.sin(az), np.cos(az), 0], [0, 0, 1]])
    return rz @ ry @ rx


def rotated_start(params: Any, center: Any, angles_deg: Any) -> tuple[list[float], list[float]]:
    """The ITK affine ``(params, center)`` preceded by a rotation about ``center``: the start
    ``x -> A R (x - c) + c + t`` of a multi-start."""
    p = np.asarray(params, dtype=np.float64).ravel()
    a = p[:9].reshape(3, 3) @ rotation_matrix(angles_deg)
    return [*a.ravel().tolist(), *p[9:].tolist()], [float(v) for v in np.ravel(center)]


def map_affine(params: Any, center: Any, points: np.ndarray) -> np.ndarray:
    """Points (N x 3) moved by the ITK affine ``(params, center)``: ``A (x - c) + c + t``, with
    ``params`` the row-major matrix and the translation (fixed to moving points)."""
    p = np.asarray(params, dtype=np.float64).ravel()
    c = np.asarray(center, dtype=np.float64).ravel()
    if p.size != 12 or c.size != 3:
        raise ValueError("an affine needs 12 parameters and a 3-element centre")
    pts = np.atleast_2d(np.asarray(points, dtype=np.float64))
    return (pts - c) @ p[:9].reshape(3, 3).T + c + p[9:]


def read_itk_affine(path: Path) -> tuple[list[float], list[float]]:
    """``(params, center)`` of an ITK ``.mat`` affine as ``antsRegistration`` writes it
    (``AffineTransform_double_3_3`` or another 12-parameter matrix-offset transform)."""
    from scipy.io import loadmat

    m = loadmat(str(path))
    center = np.asarray(m["fixed"], dtype=np.float64).ravel()
    for k, v in m.items():
        if k.startswith("__") or k == "fixed":
            continue
        params = np.asarray(v, dtype=np.float64).ravel()
        if params.size == 12:
            return params.tolist(), center.tolist()
    raise ValueError(f"{path}: no 12-parameter transform")


def mask_points(mask: np.ndarray, origin: Any, spacing: Any, direction: Any, step: int = 2):
    """Physical points (N x 3) of every ``step``-th voxel per axis of a boolean mask."""
    idx = np.argwhere(mask[::step, ::step, ::step]).astype(np.float64) * step
    m = metrics.index_to_physical_matrix(spacing, direction)
    return idx @ m.T + np.asarray(origin, dtype=np.float64)


def affine_distance(a: dict[str, Any], b: dict[str, Any], points: np.ndarray) -> dict[str, float]:
    """Mean and max distance (mm) between where two affines move ``points``."""
    if not len(points):
        return {"mean_mm": math.nan, "max_mm": math.nan}
    d = np.linalg.norm(
        map_affine(a["parameters"], a["center"], points)
        - map_affine(b["parameters"], b["center"], points),
        axis=1,
    )
    return {"mean_mm": float(d.mean()), "max_mm": float(d.max())}


def linear_done(folder: Path, cfg: dict[str, Any], iso_mm: float) -> bool:
    """Whether an engine folder holds a successful linear run of ``cfg`` at ``iso_mm``."""
    record = _record(folder)
    return (
        record is not None
        and record.get("status") == "ok"
        and record.get("kind") == "linear"
        and same_config(
            {"recipe": record.get("config"), "iso_mm": record.get("iso_mm")}, cfg, iso_mm
        )
    )


def _fit(r: dict[str, Any], wall: float) -> dict[str, Any]:
    return {
        "seconds": r.get("total_s"),
        "wall_s": wall,
        "parameters": r["parameters"],
        "center": r["center"],
        "metric": r.get("metric"),
        "iterations": r.get("iterations"),
    }


def _metric(f: dict[str, Any]) -> float:
    m = f.get("metric")
    return float(m) if isinstance(m, (int, float)) and math.isfinite(m) else math.inf


def multistart(session: Any, cfg: dict[str, Any]) -> dict[str, Any]:
    """The multi-start of :data:`MULTISTART` in an open ``LinearSession``."""
    ms = cfg["multistart"]
    coarse = coarse_stage(cfg["rigid"], int(ms["coarse_levels"]))
    p0, c0 = session.com_initial
    t0 = time.perf_counter()
    starts = []
    for angles in ms["rotations_deg"]:
        t = time.perf_counter()
        r = session.register(
            stage=coarse,
            transform="rigid",
            initial=rotated_start(p0, c0, angles),
            use_mask=False,
        )
        starts.append({"angles_deg": list(angles), **_fit(r, time.perf_counter() - t)})
    best = sorted(range(len(starts)), key=lambda i: _metric(starts[i]))[: int(ms["refine"])]
    refined = []
    for i in best:
        t = time.perf_counter()
        r = session.register(
            stage=cfg["rigid"],
            transform="rigid",
            initial=(starts[i]["parameters"], starts[i]["center"]),
            use_mask=False,
        )
        refined.append({"start": i, **_fit(r, time.perf_counter() - t)})
    chosen = min(refined, key=_metric)
    fits = [*starts, *refined]
    return {
        "seconds": float(sum(f["seconds"] or 0.0 for f in fits)),
        "wall_s": time.perf_counter() - t0,
        "n_fits": len(fits),
        "starts": starts,
        "refined": refined,
        "chosen_start": chosen["start"],
        "parameters": chosen["parameters"],
        "center": chosen["center"],
        "metric": chosen["metric"],
    }


def run_fastwarp_linear(
    fixed: Path, moving: Path, out: Path, cfg: dict[str, Any], *, adapter: str | None
) -> dict[str, Any]:
    """FastWarp's fits of one pair in one ``linear_session``; returns the ``run.json``."""
    import ants
    import fastwarp

    record: dict[str, Any] = {"kind": "linear", "engine": "fastwarp", "started": _now()}
    fixed_img, moving_img = ants.image_read(str(fixed)), ants.image_read(str(moving))
    t0 = time.perf_counter()
    try:
        with fastwarp.linear_session(
            fixed_img, moving_img, winsorize=tuple(cfg["winsorize"]), adapter=adapter
        ) as s:
            record["prepare_s"] = time.perf_counter() - t0
            record["adapter"] = s.adapter
            t = time.perf_counter()
            rigid = s.register(
                stage=cfg["rigid"], transform="rigid", use_mask=False, out_prefix=out / "rigid_"
            )
            fits = {"rigid": _fit(rigid, time.perf_counter() - t)}
            t = time.perf_counter()
            affine = s.register(
                stage=cfg["affine"],
                transform="affine",
                initial=(rigid["parameters"], rigid["center"]),
                use_mask=False,
                out_prefix=out / "affine_",
            )
            fits["affine"] = _fit(affine, time.perf_counter() - t)
            fits["multistart"] = multistart(s, cfg)
    except (fastwarp.GpuUnavailableError, fastwarp.GpuCapacityError) as e:
        record.update(status="skipped", reason=str(e))
        return record
    except Exception as e:
        record.update(status="failed", error=f"{type(e).__name__}: {e}")
        return record
    record.update(status="ok", fits=fits, wall_s=time.perf_counter() - t0, finished=_now())
    return record


def run_ants_linear(
    fixed: Path,
    moving: Path,
    out: Path,
    cfg: dict[str, Any],
    *,
    threads: int | None,
    ants_dir: Path | None,
    timeout_s: float | None,
) -> dict[str, Any]:
    """ANTs' rigid fit and its affine fit from the rigid result, two ``antsRegistration``
    processes one after the other; returns the ``run.json``."""
    record: dict[str, Any] = {
        "kind": "linear",
        "engine": "ants",
        "started": _now(),
        "program": "ANTsPy" if ants_dir is None else ants_program(ants_dir)[0],
        "itk_threads": threads,
        "fits": {},
    }
    env = child_env(threads)
    t0 = time.perf_counter()
    initial = None
    for fit in ("rigid", "affine"):
        prefix = out / f"{fit}_"
        argv = recipes.ants_linear_argv(
            cfg[fit], fit, fixed.as_posix(), moving.as_posix(), prefix.as_posix(), initial
        )
        log = out / f"{fit}.log"
        code, wall = run_child([*ants_program(ants_dir), *argv], log, env, timeout_s)
        mat = out / f"{fit}_0GenericAffine.mat"
        t = timing.ants_registration_timings(
            log.read_text(encoding="utf-8", errors="replace"), n_stages=1
        )
        entry: dict[str, Any] = {
            "seconds": t["linear_s"],
            "ants_total_s": t["total_s"],
            "wall_s": wall,
            "exit_code": code,
            "argv": argv,
        }
        if "note" in t:
            entry["timing_note"] = t["note"]
        record["fits"][fit] = entry
        if code != 0 or not mat.is_file():
            why = "timed out" if code is None else f"exited with {code}"
            record.update(status="failed", error=f"antsRegistration ({fit}) {why}; see {log.name}")
            return record
        entry["parameters"], entry["center"] = read_itk_affine(mat)
        initial = mat.as_posix()
    record.update(status="ok", wall_s=time.perf_counter() - t0, finished=_now())
    return record


def compare(
    ds: datasets.Dataset,
    folder: Path,
    prep: dict[str, Any],
    *,
    data: Path,
    cfg: dict[str, Any],
    iso_mm: float,
) -> dict[str, Any]:
    """Landmark errors of each engine's fits of ``cfg`` at ``iso_mm`` and the distance between
    the engines' fits over the fixed brain mask, for a pair folder; written as
    ``comparison.json``."""
    import ants

    pair = json.loads((folder / "pair.json").read_text(encoding="utf-8"))
    fits = {
        e: (_record(folder / e) or {}).get("fits", {})
        for e in ENGINES
        if linear_done(folder / e, cfg, iso_mm)
    }
    out: dict[str, Any] = {
        "dataset": ds.key,
        "pair": folder.name,
        "fixed": pair["fixed"],
        "moving": pair["moving"],
        "config": cfg,
        "iso_mm": iso_mm,
        "fits": {},
    }
    mask_img = ants.image_read(prep["files"]["mask"])
    pts = mask_points(mask_img.numpy() > 0, mask_img.origin, mask_img.spacing, mask_img.direction)
    fixed_s, moving_s = ds.subject(pair["fixed"]), ds.subject(pair["moving"])
    lm = None
    if fixed_s.landmarks and moving_s.landmarks:
        root = data / ds.key
        a = landmarks.read_fcsv(root / fixed_s.landmarks)
        b = landmarks.read_fcsv(root / moving_s.landmarks)
        lm = landmarks.matched(a, b)
        if len(lm[0]) < min(len(a), len(b)):
            raise ValueError(f"{folder.name}: the landmark labels of the two files differ")
        out["n_landmarks"] = len(lm[0])
    for fit in FITS:
        row: dict[str, Any] = {}
        for engine, f in fits.items():
            if fit in f and lm is not None:
                err = metrics.landmark_errors(
                    map_affine(f[fit]["parameters"], f[fit]["center"], lm[2]), lm[3]
                )
                row[engine] = {
                    "landmark_mean_mm": float(err.mean()),
                    "landmark_median_mm": float(np.median(err)),
                }
        reference = "rigid" if fit == "multistart" else fit
        if fit in fits.get("fastwarp", {}) and reference in fits.get("ants", {}):
            row["agreement"] = {
                "ants_fit": reference,
                **affine_distance(fits["fastwarp"][fit], fits["ants"][reference], pts),
            }
        out["fits"][fit] = row
    return out


def run_linear(
    *,
    data: Path,
    work: Path,
    keys: list[str],
    recipe_name: str = "cohort",
    pairs: int | None = None,
    quick: bool = False,
    engines: tuple[str, ...] = ENGINES,
    iso_mm: float = 1.0,
    threads: int | None = None,
    ants_dir: Path | None = None,
    adapter: str | None = None,
    retry_failed: bool = False,
    ants_timeout_s: float | None = 3600.0,
    log: Callable[[str], None] = print,
) -> list[dict[str, Any]]:
    """Run the linear fits of every selected pair with every engine not done yet (resumable,
    as :func:`run.run`); returns one row per pair and engine with its status."""
    for e in engines:
        if e not in ENGINES:
            raise ValueError(f"unknown engine {e!r}")
    cfg = config(recipes.recipe(recipe_name))
    rows: list[dict[str, Any]] = []
    caller = thread_env()
    with RunLock(work), rayon_threads(threads):
        env = environment(ants_dir=ants_dir, threads=threads, adapter=adapter, caller_env=caller)
        _write_json(work / "environment_linear.json", env)
        for key in keys:
            ds = datasets.get(key)
            if not (data / ds.key / ".fetched.json").is_file():
                raise RuntimeError(f"{ds.key} is not fetched into {data}; run fetch first")
            n = pairs if pairs is not None else (ds.quick_pairs if quick else ds.default_pairs)
            selected = select_pairs(ds, n)
            log(f"{ds.key}: linear fits of {len(selected)} pairs")
            for i, (fixed_id, moving_id) in enumerate(selected, 1):
                folder = work / ds.key / FOLDER / pair_id(fixed_id, moving_id)
                todo = []
                for engine in engines:
                    record = _record(folder / engine)
                    status = None if record is None else record.get("status")
                    same = record is not None and same_config(
                        {"recipe": record.get("config"), "iso_mm": record.get("iso_mm")},
                        cfg,
                        iso_mm,
                    )
                    if linear_done(folder / engine, cfg, iso_mm) or (
                        same and status in ("failed", "skipped") and not retry_failed
                    ):
                        rows.append(_row(ds.key, folder.name, engine, status, False))
                    else:
                        todo.append(engine)
                compared = _read_json(folder / "comparison.json")
                current = compared.get("config") == cfg and compared.get("iso_mm") == iso_mm
                finished = any(linear_done(folder / e, cfg, iso_mm) for e in engines)
                if not todo and (not finished or (current and "error" not in compared)):
                    continue
                try:
                    with itk_threads(threads):
                        prep = prepare_fixed(ds, fixed_id, data, work, iso_mm)
                        moving = prepare_moving(ds, moving_id, data, work, iso_mm)
                except Exception as e:
                    error = f"preparing the images failed: {type(e).__name__}: {e}"
                    log(f"[{i}/{len(selected)}] {folder.name}: {error}")
                    for engine in todo:
                        failed = {"kind": "linear", "engine": engine, "status": "failed"}
                        failed.update(error=error, config=cfg, iso_mm=iso_mm, environment=env)
                        _write_json(folder / engine / "run.json", failed)
                        rows.append(_row(ds.key, folder.name, engine, "failed", True))
                    if not todo:
                        rows.append(_row(ds.key, folder.name, "comparison", "failed", True))
                    continue
                fixed = Path(prep["files"]["registration"])
                _write_json(
                    folder / "pair.json",
                    {
                        "dataset": ds.key,
                        "fixed": fixed_id,
                        "moving": moving_id,
                        "config": cfg,
                        "iso_mm": iso_mm,
                        "fixed_prep": prep,
                        "moving_registration": str(moving),
                    },
                )
                for engine in todo:
                    out = folder / engine
                    out.mkdir(parents=True, exist_ok=True)
                    log(f"[{i}/{len(selected)}] {folder.name} linear {engine} ...")
                    try:
                        if engine == "fastwarp":
                            with itk_threads(threads):
                                record = run_fastwarp_linear(
                                    fixed, moving, out, cfg, adapter=adapter
                                )
                        else:
                            record = run_ants_linear(
                                fixed,
                                moving,
                                out,
                                cfg,
                                threads=threads,
                                ants_dir=ants_dir,
                                timeout_s=ants_timeout_s,
                            )
                    except Exception as e:
                        record = {
                            "kind": "linear",
                            "engine": engine,
                            "status": "failed",
                            "error": f"{type(e).__name__}: {e}",
                        }
                    record.update(
                        config=cfg,
                        iso_mm=iso_mm,
                        fixed_image=str(fixed),
                        moving_image=str(moving),
                        environment=env,
                    )
                    _write_json(out / "run.json", record)
                    log(f"    {record['status']} in {record.get('wall_s', math.nan):.1f} s")
                    rows.append(_row(ds.key, folder.name, engine, record["status"], True))
                try:
                    result = compare(ds, folder, prep, data=data, cfg=cfg, iso_mm=iso_mm)
                except Exception as e:
                    error = f"{type(e).__name__}: {e}"
                    log(f"    comparison failed: {error}")
                    result = {"error": error, "config": cfg, "iso_mm": iso_mm}
                    rows.append(_row(ds.key, folder.name, "comparison", "failed", True))
                _write_json(folder / "comparison.json", result)
    return rows


def _row(dataset: str, pair: str, engine: str, status: str | None, ran: bool) -> dict:
    return {"dataset": dataset, "pair": pair, "engine": engine, "status": status, "ran": ran}


def todo_count(
    work: Path, keys: list[str], recipe_name: str, pairs: int | None, quick: bool, iso_mm: float
) -> dict[str, dict[str, Any]]:
    """Per dataset: the number of selected pairs and of linear runs left per engine."""
    cfg = config(recipes.recipe(recipe_name))
    out = {}
    for key in keys:
        ds = datasets.get(key)
        n = pairs if pairs is not None else (ds.quick_pairs if quick else ds.default_pairs)
        selected = select_pairs(ds, n)
        left = {e: 0 for e in ENGINES}
        for f, m in selected:
            folder = work / ds.key / FOLDER / pair_id(f, m)
            for e in ENGINES:
                if not linear_done(folder / e, cfg, iso_mm):
                    left[e] += 1
        out[key] = {"pairs": len(selected), "to_run": left}
    return out


def _sum(values: list[Any]) -> float | None:
    if any(not isinstance(v, (int, float)) for v in values):
        return None
    return float(sum(values))


def rows(work: Path, recipe_name: str | None = None) -> list[dict[str, Any]]:
    """One row per dataset, grid, pair, engine and fit of the finished linear runs in ``work``
    (of ``recipe_name``'s linear stages, if given): seconds, landmark errors, agreement with
    ANTs and the machine fingerprint. ``prepare`` is FastWarp's session preparation;
    ``rigid_affine`` the like-for-like sum (FastWarp: preparation, rigid and affine fits; ANTs:
    the rigid and affine stages)."""
    cfg = None if recipe_name is None else config(recipes.recipe(recipe_name))
    out = []
    for path, rec in timing.read_records(work.glob(f"*/{FOLDER}/*/*/run.json")):
        if rec.get("status") != "ok" or rec.get("kind") != "linear":
            continue
        if cfg is not None and rec.get("config") != cfg:
            continue
        engine = rec.get("engine")
        folder = path.parents[1]
        comparison = _read_json(folder / "comparison.json")
        if comparison.get("config") != rec.get("config") or comparison.get("iso_mm") != rec.get(
            "iso_mm"
        ):
            comparison = {}
        compared = comparison.get("fits", {})
        base = {
            "dataset": folder.parents[1].name,
            "iso_mm": rec.get("iso_mm"),
            "pair": folder.name,
            "engine": engine,
            **timing.fingerprint(rec.get("environment")),
        }
        fits = rec.get("fits", {})
        if engine == "fastwarp" and rec.get("prepare_s") is not None:
            out.append({**base, "fit": "prepare", "seconds": rec["prepare_s"]})
        parts = [fits.get(k, {}).get("seconds") for k in ("rigid", "affine")]
        if engine == "fastwarp":
            parts.append(rec.get("prepare_s"))
        out.append({**base, "fit": "rigid_affine", "seconds": _sum(parts)})
        for fit, f in fits.items():
            c = compared.get(fit, {})
            agreement = (c.get("agreement") or {}) if engine == "fastwarp" else {}
            out.append(
                {
                    **base,
                    "fit": fit,
                    "seconds": f.get("seconds"),
                    "wall_s": f.get("wall_s"),
                    "landmark_mean_mm": (c.get(engine) or {}).get("landmark_mean_mm"),
                    "agreement_mean_mm": agreement.get("mean_mm"),
                    "agreement_max_mm": agreement.get("max_mm"),
                }
            )
    return out
