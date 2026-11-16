# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Post-processing of finished registrations: FastWarp's ``compose_field`` and
``apply_transforms`` against ``antsApplyTransforms`` on the same composite transforms.

Per pair, three steps, each run once by FastWarp (in this process) and once by
``antsApplyTransforms`` (a child process, one at a time, never at the same time as FastWarp):

- ``forward_field``: the forward composite as one displacement field on the fixed image's
  evaluation grid (``antsApplyTransforms --output [<file>,1]``);
- ``inverse_field``: the inverse composite as one displacement field on the moving image's own
  grid;
- ``lanczos_warp``: the moving image resampled onto the fixed evaluation grid through the forward
  composite with windowed-sinc (Lanczos) interpolation.

Before either tool runs, the inputs are read once, untimed, so that both find them in the
operating system's file cache (FastWarp runs first and would otherwise pay for the cold
reads). Each time covers reading the inputs and writing the output, as the ANTs command does. The
outputs are uncompressed ``.nii`` files, so the times compare the computation and a raw write;
with ``gzip`` the steps run again writing ``.nii.gz`` (FastWarp's ``compose_field`` compresses
in parallel at level 1, ITK, which writes ANTs' outputs and FastWarp's re-warped image, on one
thread at its default level). ANTs runs with ``--float 1``, so both write ``float32``. The
composites are those of the FastWarp registration of the pair (ANTs' when FastWarp has none),
the same file for both tools. Both use all cores unless ``threads`` caps them (FastWarp through
``RAYON_NUM_THREADS``, ANTs through ``ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS``). The ``.nii`` outputs
are compared (field differences in mm, the warped images' relative RMS difference), then all
outputs are deleted; only ``post.json`` stays in the pair folder.
"""

from __future__ import annotations

import json
import math
import re
import shutil
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import numpy as np

from . import datasets, metrics, recipes, timing
from .run import (
    RunLock,
    _now,
    _read_json,
    _write_json,
    ants_tool,
    child_env,
    composite,
    engine_done,
    environment,
    inverse_composite,
    itk_threads,
    pair_id,
    rayon_threads,
    run_child,
    select_pairs,
    thread_env,
    thread_setting,
)

STEPS = ("forward_field", "inverse_field", "lanczos_warp")
#: Output formats: uncompressed (always, the primary comparison) and gzip (with ``gzip``).
FORMATS = ("nii", "nii.gz")
INTERPOLATION = "lanczosWindowedSinc"
_MARK = "FASTWARP_BENCH_SECONDS"
ANTSPY_APPLY = (
    "import sys, time, ants; t = time.perf_counter(); "
    "r = ants.lib.antsApplyTransforms(sys.argv[1:]); "
    f"print('{_MARK}', repr(time.perf_counter() - t)); sys.exit(int(r or 0))"
)
_SECONDS = re.compile(rf"^{_MARK} (\S+)\s*$", re.MULTILINE)


def config(rec: dict[str, Any], iso_mm: float, gzip: bool = False) -> dict[str, Any]:
    """What a post-processing record depends on: the registrations' recipe and grid, the
    interpolation and the output formats."""
    return {
        "recipe": rec,
        "iso_mm": iso_mm,
        "interpolation": INTERPOLATION,
        "outputs": list(FORMATS if gzip else FORMATS[:1]),
        "ants_float": True,
        "warm_up": True,
    }


def source_stamp(folder: Path, engine: str) -> str | None:
    """When the registration whose composites are post-processed finished (its ``run.json``)."""
    return _read_json(folder / engine / "run.json").get("finished")


def same_source(folder: Path, record: dict[str, Any], cfg: dict[str, Any]) -> bool:
    """Whether ``record`` (a ``post.json``) was made from the registration that is in the pair
    folder now: the same engine, finished at the same time."""
    src = source_engine(folder, cfg["recipe"], cfg["iso_mm"])
    stamp = None if src is None else source_stamp(folder, src)
    return src == record.get("source_engine") and stamp == record.get("source_finished")


def post_done(folder: Path, cfg: dict[str, Any]) -> bool:
    """Whether a pair folder holds a successful ``post.json`` of ``cfg``, made from the
    registration that is there now."""
    rec = _read_json(folder / "post.json")
    return rec.get("status") == "ok" and rec.get("config") == cfg and same_source(folder, rec, cfg)


def source_engine(folder: Path, rec: dict[str, Any], iso_mm: float) -> str | None:
    """The engine whose composites are post-processed: FastWarp's, else ANTs', else ``None``."""
    for engine in ("fastwarp", "ants"):
        if engine_done(folder / engine, rec, iso_mm):
            return engine
    return None


def ants_args(
    step: str, *, reference: str, moving: str, forward: str, inverse: str, out: str
) -> list[str]:
    """The ``antsApplyTransforms`` arguments of one step."""
    paths = [recipes._check_path(p) for p in (reference, moving, forward, inverse, out)]
    reference, moving, forward, inverse, out = paths
    head = ["--dimensionality", "3", "--float", "1"]
    if step == "forward_field":
        body = ["--reference-image", reference, "--transform", forward, "--output", f"[{out},1]"]
    elif step == "inverse_field":
        body = ["--reference-image", moving, "--transform", inverse, "--output", f"[{out},1]"]
    elif step == "lanczos_warp":
        body = ["--input", moving, "--reference-image", reference, "--transform", forward]
        body += ["--interpolation", "LanczosWindowedSinc", "--output", out]
    else:
        raise ValueError(f"unknown step {step!r}")
    return [*head, *body]


def child_seconds(log_text: str) -> float | None:
    """The time the ANTsPy child measured around its ``antsApplyTransforms`` call."""
    m = _SECONDS.search(log_text)
    if not m:
        return None
    try:
        return float(m.group(1))
    except ValueError:
        return None


def warm_up(paths: list[Path], chunk: int = 1 << 24) -> int:
    """Read ``paths`` once, untimed, so that neither tool pays for reading them from disk
    first; returns the bytes read."""
    total = 0
    for path in paths:
        with open(path, "rb") as f:
            while block := f.read(chunk):
                total += len(block)
    return total


def output(tmp: Path, tool: str, step: str, fmt: str) -> Path:
    """The file a tool writes for a step in a format."""
    return tmp / f"{tool}_{step}.{fmt}"


def run_fastwarp_steps(
    reference: Path,
    moving: Path,
    forward: Path,
    inverse: Path,
    tmp: Path,
    formats: tuple[str, ...] = FORMATS[:1],
) -> dict[str, Any]:
    """FastWarp's three steps in each format; returns
    ``{"steps": {step: {fmt: {"seconds", "voxels", "bytes"}}}, "wall_s"}``."""
    import ants
    import fastwarp

    steps: dict[str, Any] = {step: {} for step in STEPS}
    t0 = time.perf_counter()
    for fmt in formats:
        t = time.perf_counter()
        ref = ants.image_read(str(reference))
        out = output(tmp, "fastwarp", "forward_field", fmt)
        fastwarp.compose_field([str(forward)], ref, out)
        steps["forward_field"][fmt] = {
            "seconds": time.perf_counter() - t,
            "voxels": int(np.prod(ref.shape)),
            "bytes": out.stat().st_size,
        }
        t = time.perf_counter()
        mov = ants.image_read(str(moving))
        out = output(tmp, "fastwarp", "inverse_field", fmt)
        fastwarp.compose_field([str(inverse)], mov, out)
        steps["inverse_field"][fmt] = {
            "seconds": time.perf_counter() - t,
            "voxels": int(np.prod(mov.shape)),
            "bytes": out.stat().st_size,
        }
        t = time.perf_counter()
        ref = ants.image_read(str(reference))
        mov = ants.image_read(str(moving))
        warped = fastwarp.apply_transforms(mov, ref, [str(forward)], interpolation=INTERPOLATION)
        if not hasattr(warped, "new_image_like"):
            warped = ref.new_image_like(np.asarray(warped, dtype=np.float32))
        out = output(tmp, "fastwarp", "lanczos_warp", fmt)
        ants.image_write(warped.clone("float"), str(out))
        steps["lanczos_warp"][fmt] = {
            "seconds": time.perf_counter() - t,
            "voxels": int(np.prod(ref.shape)),
            "bytes": out.stat().st_size,
        }
    return {"steps": steps, "wall_s": time.perf_counter() - t0}


def run_ants_steps(
    reference: Path,
    moving: Path,
    forward: Path,
    inverse: Path,
    tmp: Path,
    *,
    threads: int | None,
    ants_dir: Path | None,
    timeout_s: float | None,
    formats: tuple[str, ...] = FORMATS[:1],
) -> dict[str, Any]:
    """``antsApplyTransforms`` for the three steps in each format, one process after the other.
    A step's ``seconds`` is the process wall time for an ANTs program, and the time around the
    library call for ANTsPy (whose process start and import are not part of the step)."""
    cmd = ants_tool(ants_dir, "antsApplyTransforms", ANTSPY_APPLY)
    env = child_env(threads)
    out: dict[str, Any] = {
        "program": "ANTsPy" if ants_dir is None else cmd[0],
        "itk_threads": threads,
        "steps": {step: {} for step in STEPS},
    }
    t0 = time.perf_counter()
    for fmt in formats:
        for step in STEPS:
            target = output(tmp, "ants", step, fmt)
            args = ants_args(
                step,
                reference=reference.as_posix(),
                moving=moving.as_posix(),
                forward=forward.as_posix(),
                inverse=inverse.as_posix(),
                out=target.as_posix(),
            )
            log = tmp / f"ants_{step}.{fmt}.log"
            code, wall = run_child([*cmd, *args], log, env, timeout_s)
            seconds = wall
            entry: dict[str, Any] = {"wall_s": wall, "exit_code": code, "args": args}
            if ants_dir is None:
                seconds = child_seconds(log.read_text(encoding="utf-8", errors="replace"))
                if seconds is None:
                    seconds = wall
                    entry["timing_note"] = "no time from the ANTsPy child; process wall time used"
            if target.is_file():
                entry["bytes"] = target.stat().st_size
            out["steps"][step][fmt] = {"seconds": seconds, **entry}
            if code != 0 or not target.is_file():
                why = "timed out" if code is None else f"exited with {code}"
                text = log.read_text(encoding="utf-8", errors="replace")[-2000:]
                out["error"] = f"antsApplyTransforms ({step}, .{fmt}) {why}: {text}"
                out["wall_s"] = time.perf_counter() - t0
                return out
    out["wall_s"] = time.perf_counter() - t0
    return out


def image_difference(a: np.ndarray, b: np.ndarray) -> dict[str, float]:
    """RMS and max of ``a - b``, and the RMS relative to the RMS of ``b``."""
    if a.shape != b.shape:
        raise ValueError(f"images differ in shape: {a.shape} vs {b.shape}")
    d = a.astype(np.float64) - b.astype(np.float64)
    rms = float(np.sqrt(np.mean(d**2))) if d.size else math.nan
    ref = float(np.sqrt(np.mean(b.astype(np.float64) ** 2))) if b.size else math.nan
    return {
        "rms": rms,
        "max_abs": float(np.abs(d).max()) if d.size else math.nan,
        "rms_relative": rms / ref if ref else math.nan,
    }


def agreement(tmp: Path) -> dict[str, Any]:
    """FastWarp's uncompressed outputs against ANTs' in ``tmp``."""
    import ants

    def read(tool: str, step: str):
        return ants.image_read(str(output(tmp, tool, step, FORMATS[0]))).numpy()

    out: dict[str, Any] = {}
    for step in ("forward_field", "inverse_field"):
        out[step] = metrics.field_difference(read("fastwarp", step), read("ants", step))
    out["lanczos_warp"] = image_difference(
        read("fastwarp", "lanczos_warp"), read("ants", "lanczos_warp")
    )
    return out


def post_pair(
    ds: datasets.Dataset,
    folder: Path,
    cfg: dict[str, Any],
    *,
    data: Path,
    threads: int | None,
    ants_dir: Path | None,
    timeout_s: float | None,
    fastwarp_steps: Callable[..., dict[str, Any]] = run_fastwarp_steps,
    ants_steps: Callable[..., dict[str, Any]] = run_ants_steps,
    compare: Callable[[Path], dict[str, Any]] = agreement,
) -> dict[str, Any]:
    """Post-process one pair folder; returns the ``post.json`` content."""
    pair = json.loads((folder / "pair.json").read_text(encoding="utf-8"))
    record: dict[str, Any] = {"kind": "post", "started": _now(), "config": cfg}
    src = source_engine(folder, cfg["recipe"], cfg["iso_mm"])
    if src is None:
        record.update(status="skipped", reason="no finished registration in this pair")
        return record
    reference = Path(pair["fixed_prep"]["files"]["reference"])
    moving = data / ds.key / ds.subject(pair["moving"]).image
    forward, inverse = composite(folder / src), inverse_composite(folder / src)
    record.update(
        source_engine=src,
        source_finished=source_stamp(folder, src),
        reference=str(reference),
        moving=str(moving),
        forward=str(forward),
        inverse=str(inverse),
    )
    formats = tuple(cfg["outputs"])
    tmp = folder / "post_tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    try:
        if cfg.get("warm_up"):
            record["warm_up_bytes"] = warm_up([reference, moving, forward, inverse])
        try:
            with rayon_threads(threads), itk_threads(threads):
                record["fastwarp"] = fastwarp_steps(
                    reference, moving, forward, inverse, tmp, formats
                )
        except Exception as e:
            record.update(status="failed", error=f"FastWarp: {type(e).__name__}: {e}")
            return record
        record["ants"] = ants_steps(
            reference,
            moving,
            forward,
            inverse,
            tmp,
            threads=threads,
            ants_dir=ants_dir,
            timeout_s=timeout_s,
            formats=formats,
        )
        if "error" in record["ants"]:
            record.update(status="failed", error=record["ants"]["error"])
            return record
        try:
            with itk_threads(threads):
                record["agreement"] = compare(tmp)
        except Exception as e:
            record.update(status="failed", error=f"comparing the outputs: {type(e).__name__}: {e}")
            return record
        record.update(status="ok", finished=_now(), threads=thread_setting(threads))
        return record
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def run_post(
    *,
    data: Path,
    work: Path,
    keys: list[str],
    recipe_name: str = "cohort",
    pairs: int | None = None,
    quick: bool = False,
    iso_mm: float = 1.0,
    threads: int | None = None,
    ants_dir: Path | None = None,
    retry_failed: bool = False,
    ants_timeout_s: float | None = 3600.0,
    gzip: bool = False,
    log: Callable[[str], None] = print,
    **step_overrides: Any,
) -> list[dict[str, Any]]:
    """Post-process every selected pair whose registration finished and that is not done yet
    (resumable); returns one row per pair with its status. ``threads`` caps both tools' CPU
    threads (``None``: all cores), set before FastWarp is first used here (see
    :func:`run.rayon_threads`)."""
    rec = recipes.recipe(recipe_name)
    cfg = config(rec, iso_mm, gzip)
    rows: list[dict[str, Any]] = []
    caller = thread_env()
    with RunLock(work), rayon_threads(threads):
        env = environment(ants_dir=ants_dir, threads=threads, adapter=None, caller_env=caller)
        _write_json(work / "environment_post.json", env)
        for key in keys:
            ds = datasets.get(key)
            n = pairs if pairs is not None else (ds.quick_pairs if quick else ds.default_pairs)
            selected = select_pairs(ds, n)
            log(f"{ds.key}: post-processing of {len(selected)} pairs")
            for i, (fixed_id, moving_id) in enumerate(selected, 1):
                folder = work / ds.key / recipe_name / pair_id(fixed_id, moving_id)
                old = _read_json(folder / "post.json")
                same = old.get("config") == cfg and same_source(folder, old, cfg)
                if post_done(folder, cfg) or (
                    same and old.get("status") in ("failed", "skipped") and not retry_failed
                ):
                    rows.append(_row(ds.key, folder.name, old.get("status"), False))
                    continue
                if (
                    not (folder / "pair.json").is_file()
                    or source_engine(folder, rec, iso_mm) is None
                ):
                    log(f"[{i}/{len(selected)}] {folder.name}: no finished registration; run first")
                    rows.append(_row(ds.key, folder.name, None, False))
                    continue
                log(f"[{i}/{len(selected)}] {folder.name} post-processing ...")
                try:
                    record = post_pair(
                        ds,
                        folder,
                        cfg,
                        data=data,
                        threads=threads,
                        ants_dir=ants_dir,
                        timeout_s=ants_timeout_s,
                        **step_overrides,
                    )
                except Exception as e:
                    src = source_engine(folder, rec, iso_mm)
                    record = {
                        "kind": "post",
                        "config": cfg,
                        "status": "failed",
                        "error": f"{type(e).__name__}: {e}",
                        "source_engine": src,
                        "source_finished": None if src is None else source_stamp(folder, src),
                    }
                record["environment"] = env
                _write_json(folder / "post.json", record)
                log(f"    {record['status']}")
                rows.append(_row(ds.key, folder.name, record["status"], True))
    return rows


def _row(dataset: str, pair: str, status: str | None, ran: bool) -> dict[str, Any]:
    return {"dataset": dataset, "pair": pair, "status": status, "ran": ran}


def rows(
    work: Path, recipe_name: str, recipe: dict[str, Any] | None = None
) -> list[dict[str, Any]]:
    """One row per dataset, grid, pair, step and output format of the finished post-processing
    records in ``work/*/<recipe_name>/`` (of registrations with ``recipe``, if given): voxels,
    FastWarp and ANTs seconds and output bytes, the outputs' difference and the machine
    fingerprint."""
    out = []
    for path, rec in timing.read_records(work.glob(f"*/{recipe_name}/*/post.json")):
        if rec.get("status") != "ok":
            continue
        cfg = rec.get("config") or {}
        if recipe is not None and cfg.get("recipe") != recipe:
            continue
        fp = timing.fingerprint(rec.get("environment"))
        for step in STEPS:
            fw_step = ((rec.get("fastwarp") or {}).get("steps") or {}).get(step) or {}
            an_step = ((rec.get("ants") or {}).get("steps") or {}).get(step) or {}
            ag = (rec.get("agreement") or {}).get(step) or {}
            for fmt in cfg.get("outputs", []):
                fw, an = fw_step.get(fmt) or {}, an_step.get(fmt) or {}
                out.append(
                    {
                        "dataset": path.parents[2].name,
                        "iso_mm": cfg.get("iso_mm"),
                        "pair": path.parent.name,
                        "step": step,
                        "format": fmt,
                        "voxels": fw.get("voxels"),
                        "fastwarp_s": fw.get("seconds"),
                        "ants_s": an.get("seconds"),
                        "fastwarp_bytes": fw.get("bytes"),
                        "ants_bytes": an.get("bytes"),
                        "field_diff_rms_mm": ag.get("field_diff_rms_mm"),
                        "field_diff_max_mm": ag.get("field_diff_max_mm"),
                        "image_diff_rms_relative": ag.get("rms_relative"),
                        **fp,
                    }
                )
    return out
