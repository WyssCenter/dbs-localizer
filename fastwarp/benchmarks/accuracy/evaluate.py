# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Evaluation of finished pairs against the ground truth and between the two engines.

Both engines' composites go through the same ANTsPy calls (``antsApplyTransforms``): the
moving labels are warped onto the fixed evaluation grid with nearest-neighbour interpolation,
and the forward composite is composed into one displacement field on that grid, which moves
the fixed landmarks into the moving image (``p + u(p)``, trilinear) and gives the Jacobian
determinant. Each pair's result is cached in ``evaluation.json``; :func:`collect` writes the
CSV tables.
"""

from __future__ import annotations

import csv
import json
import math
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import numpy as np

from . import datasets, landmarks, metrics
from .run import ENGINES, engine_done, pair_id, select_pairs
from .run import composite as run_composite


def _label_groups(ds: datasets.Dataset) -> dict[int, str]:
    return {label: group for group, labels in ds.groups.items() for label in labels}


def _field(ref, composite: Path, prefix: Path) -> np.ndarray:
    import ants

    path = ants.apply_transforms(
        fixed=ref, moving=ref, transformlist=[str(composite)], compose=str(prefix)
    )
    try:
        return np.asarray(ants.image_read(str(path)).numpy(), dtype=np.float32)
    finally:
        Path(path).unlink(missing_ok=True)


def _warp(ref, moving, composite: Path) -> np.ndarray:
    import ants

    return ants.apply_transforms(
        fixed=ref, moving=moving, transformlist=[str(composite)], interpolator="nearestNeighbor"
    ).numpy()


def evaluate_pair(
    ds: datasets.Dataset,
    folder: Path,
    *,
    data: Path,
    surface: bool = True,
) -> dict[str, Any]:
    """Evaluate one pair folder (both engines where they finished)."""
    import ants

    t0 = time.perf_counter()
    pair = json.loads((folder / "pair.json").read_text(encoding="utf-8"))
    files = pair["fixed_prep"]["files"]
    ref = ants.image_read(files["reference"])
    mask = ants.image_read(files["mask"]).numpy() > 0
    fixed_s, moving_s = ds.subject(pair["fixed"]), ds.subject(pair["moving"])
    groups = _label_groups(ds)
    root = data / ds.key
    moving_labels = ants.image_read(str(root / moving_s.labels)) if moving_s.labels else None
    moving_masks = {label: ants.image_read(str(root / rel)) for label, rel in moving_s.masks}
    fixed_masks = {
        label: ants.image_read(files[f"structure_{label}"]).numpy() > 0
        for label, _ in fixed_s.masks
        if f"structure_{label}" in files
    }
    lm = None
    if fixed_s.landmarks and moving_s.landmarks:
        a = landmarks.read_fcsv(root / fixed_s.landmarks)
        b = landmarks.read_fcsv(root / moving_s.landmarks)
        lm = landmarks.matched(a, b)
        if len(lm[0]) < min(len(a), len(b)):
            raise ValueError(
                f"{folder.name}: only {len(lm[0])} of {min(len(a), len(b))} landmark labels "
                "match between the fixed and the moving file"
            )
    out: dict[str, Any] = {
        "dataset": ds.key,
        "pair": folder.name,
        "fixed": pair["fixed"],
        "moving": pair["moving"],
        "recipe_name": pair["recipe_name"],
        "iso_mm": pair.get("iso_mm"),
        "surface": surface,
        "n_landmarks": None if lm is None else len(lm[0]),
        "engines": {},
    }
    fields: dict[str, np.ndarray] = {}
    warped_maps: dict[str, np.ndarray] = {}
    warped_masks: dict[str, dict[int, np.ndarray]] = {}
    for engine in ENGINES:
        run_dir = folder / engine
        if not engine_done(run_dir, pair.get("recipe"), pair.get("iso_mm")):
            continue
        record = json.loads((run_dir / "run.json").read_text(encoding="utf-8"))
        res: dict[str, Any] = {
            "wall_s": record.get("wall_s"),
            "engine_total_s": (record.get("info") or {}).get("total_s"),
        }
        composite = run_composite(run_dir)
        rows = []

        def score(label: int, a: np.ndarray, b: np.ndarray) -> dict[str, Any]:
            row = {"label": label, **metrics.mask_metrics(a, b, ref.spacing, surface=surface)}
            row["group"] = groups.get(label, "")
            row["name"] = ds.label_names.get(label, "")
            return row

        if moving_labels is not None:
            w = _warp(ref, moving_labels, composite)
            warped_maps[engine] = w
            r = ref.numpy()
            for label in sorted(groups):
                a = r == label
                if a.any():
                    rows.append(score(label, a, w == label))
        if moving_masks:
            warped_masks[engine] = {}
            for label, m in sorted(moving_masks.items()):
                if label in fixed_masks and fixed_masks[label].any():
                    b = _warp(ref, m, composite) > 0
                    warped_masks[engine][label] = b
                    rows.append(score(label, fixed_masks[label], b))
        if rows:
            res["labels"] = rows
        field = _field(ref, composite, run_dir / "eval_")
        fields[engine] = field
        det = metrics.jacobian_determinant(field, ref.spacing, ref.direction)
        res["jacobian"] = metrics.jacobian_summary(det, mask)
        del det
        if lm is not None:
            names, descs, p_fixed, p_moving = lm
            pred = metrics.map_points(field, p_fixed, ref.origin, ref.spacing, ref.direction)
            err = metrics.landmark_errors(pred, p_moving)
            res["landmarks"] = [
                {"label": n, "description": d, "error_mm": float(e)}
                for n, d, e in zip(names, descs, err, strict=True)
            ]
        out["engines"][engine] = res
    if len(fields) == 2:
        agreement = metrics.field_difference(fields["fastwarp"], fields["ants"], mask)
        rows = []
        if len(warped_maps) == 2:
            r = ref.numpy()
            for label in sorted(groups):
                if (r == label).any():
                    a, b = warped_maps["fastwarp"] == label, warped_maps["ants"] == label
                    rows.append({"label": label, "dice": metrics.dice(a, b)})
        if len(warped_masks) == 2:
            for label in sorted(set(warped_masks["fastwarp"]) & set(warped_masks["ants"])):
                a, b = warped_masks["fastwarp"][label], warped_masks["ants"][label]
                rows.append({"label": label, "dice": metrics.dice(a, b)})
        for row in rows:
            row["group"] = groups.get(row["label"], "")
        if rows:
            agreement["labels"] = rows
        out["agreement"] = agreement
    out["wall_s"] = time.perf_counter() - t0
    return out


def evaluate(
    *,
    data: Path,
    work: Path,
    keys: list[str],
    recipe_name: str,
    pairs: int | None,
    quick: bool,
    surface: bool = True,
    force: bool = False,
    log: Callable[[str], None] = print,
) -> Path:
    """Evaluate every selected pair that has a finished engine, then :func:`collect`."""
    for key in keys:
        ds = datasets.get(key)
        n = pairs if pairs is not None else (ds.quick_pairs if quick else ds.default_pairs)
        for fixed_id, moving_id in select_pairs(ds, n):
            folder = work / ds.key / recipe_name / pair_id(fixed_id, moving_id)
            runs = [folder / e / "run.json" for e in ENGINES if engine_done(folder / e)]
            if not runs:
                continue
            target = folder / "evaluation.json"
            newest = max(r.stat().st_mtime for r in [*runs, folder / "pair.json"] if r.is_file())
            if target.is_file() and target.stat().st_mtime > newest and not force:
                cached = json.loads(target.read_text(encoding="utf-8"))
                if "error" not in cached and (cached.get("surface") or not surface):
                    continue
            log(f"evaluating {ds.key}/{folder.name}")
            try:
                result = evaluate_pair(ds, folder, data=data, surface=surface)
            except Exception as e:
                error = f"{type(e).__name__}: {e}"
                log(f"    failed: {error}")
                result = {
                    "dataset": ds.key,
                    "pair": folder.name,
                    "fixed": fixed_id,
                    "moving": moving_id,
                    "recipe_name": recipe_name,
                    "surface": surface,
                    "error": error,
                    "engines": {},
                }
            tmp = target.with_name(target.name + ".tmp")
            tmp.write_text(json.dumps(result, indent=1), encoding="utf-8")
            tmp.replace(target)
    return collect(work=work, keys=keys, recipe_name=recipe_name, pairs=pairs, quick=quick)


def _mean(values: list[float]) -> float:
    v = [x for x in values if x is not None and not math.isnan(x)]
    return float(np.mean(v)) if v else math.nan


def _write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    columns: list[str] = []
    for r in rows:
        for k in r:
            if k not in columns:
                columns.append(k)
    with open(path, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=columns)
        w.writeheader()
        w.writerows(rows)


def collect(
    *, work: Path, keys: list[str], recipe_name: str, pairs: int | None, quick: bool
) -> Path:
    """Write ``labels.csv``, ``landmarks.csv``, ``pairs.csv`` and ``agreement.csv`` under
    ``<work>/results/<recipe>/`` from the cached evaluations; returns that folder."""
    label_rows, landmark_rows, pair_rows, agreement_rows = [], [], [], []
    for key in keys:
        ds = datasets.get(key)
        n = pairs if pairs is not None else (ds.quick_pairs if quick else ds.default_pairs)
        for fixed_id, moving_id in select_pairs(ds, n):
            path = work / ds.key / recipe_name / pair_id(fixed_id, moving_id) / "evaluation.json"
            if not path.is_file():
                continue
            ev = json.loads(path.read_text(encoding="utf-8"))
            base = {
                "dataset": ds.key,
                "recipe": recipe_name,
                "pair": ev["pair"],
                "fixed": ev["fixed"],
                "moving": ev["moving"],
            }
            for engine, res in ev["engines"].items():
                row = {
                    **base,
                    "engine": engine,
                    "wall_s": res.get("wall_s"),
                    "engine_total_s": res.get("engine_total_s"),
                }
                for r in res.get("labels", []):
                    label_rows.append({**base, "engine": engine, **r})
                for group in ds.groups:
                    sel = [r for r in res.get("labels", []) if r["group"] == group]
                    if sel:
                        for m in ("dice", "jaccard", "hd95_mm", "assd_mm"):
                            row[f"{m}_{group}"] = _mean([r[m] for r in sel])
                        row[f"structures_{group}"] = len(sel)
                        row[f"missing_{group}"] = sum(1 for r in sel if r["voxels_warped"] == 0)
                lms = res.get("landmarks", [])
                for r in lms:
                    landmark_rows.append({**base, "engine": engine, **r})
                if lms:
                    errs = [r["error_mm"] for r in lms]
                    row["landmark_mean_mm"] = float(np.mean(errs))
                    row["landmark_median_mm"] = float(np.median(errs))
                    row["landmark_max_mm"] = float(np.max(errs))
                row.update(res.get("jacobian", {}))
                pair_rows.append(row)
            ag = ev.get("agreement")
            if ag:
                row = {**base, **{k: v for k, v in ag.items() if k != "labels"}}
                for group in ds.groups:
                    sel = [r["dice"] for r in ag.get("labels", []) if r["group"] == group]
                    if sel:
                        row[f"agreement_dice_{group}"] = _mean(sel)
                agreement_rows.append(row)
    out = work / "results" / recipe_name
    _write_csv(out / "labels.csv", label_rows)
    _write_csv(out / "landmarks.csv", landmark_rows)
    _write_csv(out / "pairs.csv", pair_rows)
    _write_csv(out / "agreement.csv", agreement_rows)
    return out
