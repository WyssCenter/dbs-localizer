# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Summary tables (CSV and Markdown) and an optional plot from the evaluation CSVs and the
timing records.

Every paired comparison uses the pairs where both engines finished: the per-pair value of each
engine (for example the mean Dice over a structure group), the difference FastWarp minus ANTs
with a 95 % percentile bootstrap CI of its mean, and the Wilcoxon signed-rank p-value. The
timing tables give the median and quartiles over pairs of the registration, linear-fit and
post-processing times of both engines, with the machine and software that produced them.
"""

from __future__ import annotations

import csv
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any

import numpy as np

from . import linear, post, recipes, timing
from .stats import paired_summary

#: Per-pair metrics compared between the engines, if present, and which way is better.
METRIC_PREFIXES = {
    "dice_": "higher",
    "hd95_mm_": "lower",
    "assd_mm_": "lower",
    "landmark_mean_mm": "lower",
    "landmark_median_mm": "lower",
    "jac_nonpositive_frac": "lower",
    "wall_s": "lower",
}


def _read_csv(path: Path) -> list[dict[str, str]]:
    if not path.is_file():
        return []
    with open(path, newline="", encoding="utf-8") as f:
        return list(csv.DictReader(f))


def _float(v: Any) -> float:
    try:
        return float(v)
    except (TypeError, ValueError):
        return math.nan


def _metrics(columns: list[str]) -> list[str]:
    out = []
    for c in columns:
        for prefix in METRIC_PREFIXES:
            if c == prefix or (prefix.endswith("_") and c.startswith(prefix)):
                out.append(c)
                break
    return out


def paired_table(pairs: list[dict[str, str]]) -> list[dict[str, Any]]:
    """One row per dataset and metric: :func:`paired_summary` of FastWarp against ANTs."""
    by: dict[tuple[str, str], dict[str, dict[str, str]]] = defaultdict(dict)
    columns: list[str] = []
    for r in pairs:
        by[(r["dataset"], r["pair"])][r["engine"]] = r
        for c in r:
            if c not in columns:
                columns.append(c)
    rows = []
    for dataset in sorted({d for d, _ in by}):
        both = [v for (d, _), v in sorted(by.items()) if d == dataset and len(v) == 2]
        for metric in _metrics(columns):
            a = [_float(v["fastwarp"].get(metric)) for v in both]
            b = [_float(v["ants"].get(metric)) for v in both]
            if not any(not math.isnan(x) for x in a):
                continue
            s = paired_summary(a, b)
            better = next(v for k, v in METRIC_PREFIXES.items() if metric.startswith(k))
            rows.append({"dataset": dataset, "metric": metric, "better": better, **s})
    return rows


def agreement_table(agreement: list[dict[str, str]]) -> list[dict[str, Any]]:
    """Mean, SD and max over pairs of each FastWarp-ANTs agreement column per dataset."""
    rows = []
    for dataset in sorted({r["dataset"] for r in agreement}):
        sel = [r for r in agreement if r["dataset"] == dataset]
        for col in sel[0]:
            if col in ("dataset", "recipe", "pair", "fixed", "moving"):
                continue
            v = np.array([_float(r.get(col)) for r in sel])
            v = v[~np.isnan(v)]
            if not v.size:
                continue
            rows.append(
                {
                    "dataset": dataset,
                    "metric": col,
                    "n": int(v.size),
                    "mean": float(v.mean()),
                    "sd": float(v.std(ddof=1)) if v.size > 1 else math.nan,
                    "max": float(v.max()),
                }
            )
    return rows


def per_structure_table(labels: list[dict[str, str]], metric: str = "dice") -> list[dict]:
    """Mean of ``metric`` per dataset, structure and engine over the pairs."""
    acc: dict[tuple, list[float]] = defaultdict(list)
    for r in labels:
        acc[(r["dataset"], r["group"], int(r["label"]), r.get("name", ""), r["engine"])].append(
            _float(r[metric])
        )
    keys = sorted({k[:4] for k in acc})
    rows = []
    for k in keys:
        row = {"dataset": k[0], "group": k[1], "label": k[2], "name": k[3]}
        for engine in ("fastwarp", "ants"):
            v = np.array(acc.get((*k, engine), []))
            v = v[~np.isnan(v)]
            row[f"{metric}_{engine}_mean"] = float(v.mean()) if v.size else math.nan
            row[f"{metric}_{engine}_n"] = int(v.size)
        rows.append(row)
    return rows


def _fmt(v: Any, digits: int = 3) -> str:
    if isinstance(v, float):
        if math.isnan(v):
            return "n/a"
        if v != 0 and (abs(v) < 1e-3 or abs(v) >= 1e5):
            return f"{v:.2e}"
        return f"{v:.{digits}f}"
    return str(v)


def _md_table(rows: list[dict[str, Any]], columns: list[tuple[str, str]]) -> str:
    head = "| " + " | ".join(t for _, t in columns) + " |"
    sep = "| " + " | ".join("---" for _ in columns) + " |"
    body = ["| " + " | ".join(_fmt(r.get(k)) for k, _ in columns) + " |" for r in rows]
    return "\n".join([head, sep, *body])


def _write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    if not rows:
        path.write_text("", encoding="utf-8")
        return
    columns: list[str] = []
    for r in rows:
        columns += [k for k in r if k not in columns]
    with open(path, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=columns)
        w.writeheader()
        w.writerows(rows)


def _plot(pairs: list[dict[str, str]], path: Path) -> bool:
    try:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        return False
    panels = []
    for dataset in sorted({r["dataset"] for r in pairs}):
        sel = [r for r in pairs if r["dataset"] == dataset]
        for metric in _metrics(list(sel[0])):
            if metric.startswith("dice_") or metric == "landmark_mean_mm":
                panels.append((dataset, metric, sel))
    if not panels:
        return False
    fig, axes = plt.subplots(1, len(panels), figsize=(3.2 * len(panels), 3.6), squeeze=False)
    for ax, (dataset, metric, sel) in zip(axes[0], panels, strict=True):
        data = [
            [
                v
                for v in (_float(r.get(metric)) for r in sel if r["engine"] == e)
                if not math.isnan(v)
            ]
            for e in ("fastwarp", "ants")
        ]
        ax.boxplot(data)
        ax.set_xticks([1, 2], ["FastWarp", "ANTs"])
        ax.set_title(f"{dataset}\n{metric}", fontsize=9)
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)
    return True


#: Order of the linear fits and post-processing steps in the tables.
LINEAR_FITS = ("prepare", "rigid_affine", "rigid", "affine", "multistart")
POST_STEPS = ("forward_field", "inverse_field", "lanczos_warp")
_FIT_NAMES = {
    "prepare": "session preparation",
    "rigid_affine": "rigid + affine (FastWarp with its preparation)",
    "rigid": "rigid",
    "affine": "affine (from the rigid fit)",
    "multistart": "rigid multi-start",
}
_STEP_NAMES = {
    "forward_field": "forward field (compose)",
    "inverse_field": "inverse field (compose)",
    "lanczos_warp": "Lanczos re-warp",
}
#: The commands whose environment files hold their thread settings.
COMMANDS = (
    ("run", "environment.json"),
    ("linear", "environment_linear.json"),
    ("post", "environment_post.json"),
)


def _summaries(rows: list[dict[str, Any]], keys: tuple[str, ...], fields: tuple[str, ...]):
    return {tuple(r.get(k) for k in keys): r for r in timing.summary_rows(rows, keys, fields)}


def _cell(stats: dict[str, Any], rows: int, digits: int = 1) -> str:
    text = timing.spread(stats, digits)
    if stats.get("n") and stats["n"] != rows:
        text += f" [n={stats['n']}]"
    return text


def _grid(iso_mm: Any) -> str:
    v = _float(iso_mm)
    return "n/a" if math.isnan(v) else f"{v:g} mm"


def _groups(s: dict[tuple, Any]) -> list[tuple[Any, Any, Any]]:
    return sorted({k[:3] for k in s}, key=lambda g: (str(g[0]), _float(g[1]), str(g[2])))


def _same(r: dict[str, Any], dataset: Any, iso: Any, threads: Any) -> bool:
    return r["dataset"] == dataset and r.get("iso_mm") == iso and r.get("threads") == threads


def _machine(r: dict[str, Any]) -> str:
    return json.dumps({k: r.get(k) for k in timing.fingerprint(None)}, sort_keys=True, default=str)


def engine_ratios(rows: list[dict[str, Any]], field: str) -> dict[str, Any]:
    """:func:`timing.describe` of ANTs / FastWarp of ``field`` per pair, over the pairs of
    ``rows`` (one row per pair and engine) where both engines have it and ran on the same
    machine and software (:func:`timing.fingerprint`)."""
    by: dict[str, dict[str, dict[str, Any]]] = defaultdict(dict)
    for r in rows:
        by[r["pair"]][r["engine"]] = r
    return timing.describe(
        timing.ratio(_float(v["ants"].get(field)), _float(v["fastwarp"].get(field)))
        for v in by.values()
        if "ants" in v and "fastwarp" in v and _machine(v["ants"]) == _machine(v["fastwarp"])
    )


def _ratio_cell(d: dict[str, Any]) -> str:
    if not d["n"]:
        return "n/a"
    return f"{d['median']:.1f}x ({d['n']} pair{'' if d['n'] == 1 else 's'})"


def registration_timing_table(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Per dataset, grid and engine: registrations and ``median (q1–q3)`` of the linear, SyN,
    engine total and wall seconds (``[n=k]`` when only ``k`` registrations have the value), and
    the median over pairs of ANTs / FastWarp, on the pairs where both engines have it."""
    fields = ("linear_s", "syn_s", "total_s", "wall_s")
    s = _summaries(rows, ("dataset", "iso_mm", "threads", "engine"), fields)
    out = []
    for dataset, iso, threads in _groups(s):
        for engine in ("fastwarp", "ants"):
            r = s.get((dataset, iso, threads, engine))
            if r is None:
                continue
            row: dict[str, Any] = {
                "dataset": dataset,
                "grid": _grid(iso),
                "threads": _value(threads),
                "engine": engine,
                "n": r["rows"],
            }
            for f in fields:
                row[f] = _cell(timing.stats_of(r, f), r["rows"])
            out.append(row)
        if s.get((dataset, iso, threads, "fastwarp")) and s.get((dataset, iso, threads, "ants")):
            sel = [r for r in rows if _same(r, dataset, iso, threads)]
            row = {
                "dataset": dataset,
                "grid": _grid(iso),
                "threads": _value(threads),
                "engine": "ANTs / FastWarp",
                "n": "",
            }
            for f in fields:
                row[f] = _ratio_cell(engine_ratios(sel, f))
            out.append(row)
    return out


def linear_timing_table(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Per dataset, grid and fit: the pairs of each engine, FastWarp and ANTs seconds
    (``median (q1–q3)``), the median over pairs of ANTs / FastWarp (pairs both engines have),
    each engine's median landmark error and the median distance between the engines' fits."""
    fields = ("seconds", "landmark_mean_mm", "agreement_mean_mm", "agreement_max_mm")
    s = _summaries(rows, ("dataset", "iso_mm", "threads", "fit", "engine"), fields)
    out = []
    for dataset, iso, threads in _groups(s):
        for fit in LINEAR_FITS:
            fw = s.get((dataset, iso, threads, fit, "fastwarp"))
            an = s.get((dataset, iso, threads, fit, "ants"))
            if fw is None and an is None:
                continue

            def cell(r: dict[str, Any] | None, f: str) -> str:
                return "n/a" if r is None else _cell(timing.stats_of(r, f), r["rows"], 2)

            sel = [r for r in rows if _same(r, dataset, iso, threads) and r["fit"] == fit]
            out.append(
                {
                    "dataset": dataset,
                    "grid": _grid(iso),
                    "threads": _value(threads),
                    "fit": _FIT_NAMES.get(fit, fit),
                    "n": f"{fw['rows'] if fw else 0} / {an['rows'] if an else 0}",
                    "fastwarp_s": cell(fw, "seconds"),
                    "ants_s": cell(an, "seconds"),
                    "ratio": _ratio_cell(engine_ratios(sel, "seconds")),
                    "landmark_fastwarp": cell(fw, "landmark_mean_mm"),
                    "landmark_ants": cell(an, "landmark_mean_mm"),
                    "agreement_mean": cell(fw, "agreement_mean_mm"),
                    "agreement_max": cell(fw, "agreement_max_mm"),
                }
            )
    return out


def post_timing_table(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Per dataset, grid, step and output format: voxels, ANTs and FastWarp seconds
    (``median (q1–q3)``), the median over pairs of ANTs / FastWarp, the median output sizes and
    the median difference between the two (uncompressed) outputs."""
    fields = (
        "voxels",
        "fastwarp_s",
        "ants_s",
        "fastwarp_bytes",
        "ants_bytes",
        "field_diff_max_mm",
        "image_diff_rms_relative",
    )
    s = _summaries(rows, ("dataset", "iso_mm", "threads", "step", "format"), fields)
    formats = sorted({k[4] for k in s}, key=lambda f: post.FORMATS.index(f))
    out = []
    for dataset, iso, threads in _groups(s):
        for step in POST_STEPS:
            for fmt in formats:
                r = s.get((dataset, iso, threads, step, fmt))
                if r is None:
                    continue
                if step == "lanczos_warp":
                    diff = _fmt(_float(r.get("image_diff_rms_relative_median")))
                    diff = f"relative RMS {diff}"
                else:
                    diff = f"max {_fmt(_float(r.get('field_diff_max_mm_median')))} mm"
                sel = [
                    x
                    for x in rows
                    if _same(x, dataset, iso, threads) and x["step"] == step and x["format"] == fmt
                ]
                ratios = timing.describe(
                    timing.ratio(_float(x.get("ants_s")), _float(x.get("fastwarp_s"))) for x in sel
                )
                voxels = _float(r.get("voxels_median"))
                sizes = [_float(r.get(f"{e}_bytes_median")) / 1e6 for e in ("fastwarp", "ants")]
                out.append(
                    {
                        "dataset": dataset,
                        "grid": _grid(iso),
                        "threads": _value(threads),
                        "step": _STEP_NAMES[step],
                        "format": f".{fmt}",
                        "n": r["rows"],
                        "voxels": "n/a" if math.isnan(voxels) else f"{voxels / 1e6:.1f} M",
                        "ants_s": _cell(timing.stats_of(r, "ants_s"), r["rows"], 2),
                        "fastwarp_s": _cell(timing.stats_of(r, "fastwarp_s"), r["rows"], 2),
                        "ratio": _ratio_cell(ratios),
                        "size": " / ".join("n/a" if math.isnan(v) else f"{v:.0f}" for v in sizes),
                        "difference": diff,
                    }
                )
    return out


def _value(v: Any) -> str:
    return "not recorded" if v is None or v == "" else str(v)


def machine_lines(env: dict[str, Any]) -> list[str]:
    """The hardware and software of an ``environment`` record, as Markdown list items."""
    gpu = env.get("gpu") or {}
    fw_env = env.get("fastwarp_env") or {}
    return [
        f"- CPU: {_value(env.get('cpu'))}, {_value(env.get('cpu_count'))} logical cores, "
        f"{_value(env.get('cpu_count_physical'))} physical",
        f"- GPU: {_value(gpu.get('name', gpu.get('error')))}, {_value(gpu.get('device_type'))}, "
        f"{_value(gpu.get('backend'))}, driver name {_value(gpu.get('driver'))}",
        f"- OS: {_value(env.get('platform'))}; Python {_value(env.get('python'))}, NumPy "
        f"{_value(env.get('numpy'))}, SciPy {_value(env.get('scipy'))}",
        f"- fastwarp {_value(env.get('fastwarp'))} (engine "
        f"{_value(env.get('fastwarp_engine_source'))}); ANTs {_value(env.get('ants_version'))} "
        f"({_value(env.get('ants_program'))}); ANTsPy {_value(env.get('antspyx'))}"
        + ("; " + ", ".join(f"{k}={v}" for k, v in sorted(fw_env.items())) if fw_env else ""),
        f"- benchmark commit {_value(env.get('benchmark_commit'))}"
        + (" with local changes" if env.get("benchmark_tree_dirty") else "")
        + f", recorded {_value(env.get('recorded'))}",
    ]


def thread_lines(envs: dict[str, dict[str, Any]]) -> list[str]:
    """The thread setting each command ran with (from its environment file) and the cores."""
    return [
        f"- `{command}`: threads {_value(timing.thread_label(env))} "
        f"(ANTs and FastWarp's CPU work); {_value(env.get('cpu_count'))} logical, "
        f"{_value(env.get('cpu_count_physical'))} physical cores"
        for command, env in envs.items()
    ]


def machine_table(fingerprints: list[tuple[dict[str, Any], int]]) -> list[dict[str, Any]]:
    """One row per distinct machine fingerprint with the number of records."""
    return [{**fp, "records": n} for fp, n in fingerprints]


def _timing_sections(reg: list, lin: list, pst: list) -> list[str]:
    lines: list[str] = []
    if reg:
        lines += [
            "",
            "## Registration time",
            "",
            "Seconds per registration, median (quartiles); `[n=k]`: only k registrations have "
            "the value. Linear: the rigid and affine stages; SyN: the SyN stage; total: the "
            "engine's own total (FastWarp: opening the GPU, resampling, preprocessing, both "
            "stages, warping the moving image onto the grid and writing the transforms; ANTs: "
            "`Total elapsed time`, the stages and their set-up without reading the images or "
            "writing the transforms), so the totals do not cover the same work; wall: "
            "FastWarp's `register_syn` call (images already read), ANTs' whole process (start, "
            "reading, writing; with ANTsPy also starting Python). ANTs' split comes from its "
            "`--verbose 1` log. ANTs / FastWarp: the median over pairs of the ratio, on the "
            "pairs where both engines have the value and ran on the same machine and software.",
            "",
            _md_table(
                registration_timing_table(reg),
                [
                    ("dataset", "Dataset"),
                    ("grid", "Grid"),
                    ("threads", "Threads"),
                    ("engine", "Engine"),
                    ("n", "Registrations"),
                    ("linear_s", "Linear (s)"),
                    ("syn_s", "SyN (s)"),
                    ("total_s", "Total (s)"),
                    ("wall_s", "Wall (s)"),
                ],
            ),
        ]
    if lin:
        lines += [
            "",
            "## Linear fits",
            "",
            "Seconds per fit, median (quartiles). FastWarp: one `linear_session` per pair; its "
            "preparation (opening the GPU, converting, winsorizing and uploading the images, "
            "the centre-of-mass start) is its own row and not part of the fits; rigid from the "
            "centres of mass, affine from the rigid result, then a rigid multi-start. ANTs: one "
            "`antsRegistration` stage time each, which includes the stage's own image "
            "preparation; no multi-start. Rigid + affine compares FastWarp's preparation and "
            "both fits with ANTs' two stages. Pairs: FastWarp / ANTs. ANTs / FastWarp: the "
            "median over pairs of the ratio (same machine and software). Landmark error: mean "
            "over the landmarks of a pair, "
            "mm; FastWarp - ANTs: mean and max distance between the two fits over the fixed "
            "brain mask, mm (the multi-start against ANTs' rigid fit).",
            "",
            _md_table(
                linear_timing_table(lin),
                [
                    ("dataset", "Dataset"),
                    ("grid", "Grid"),
                    ("threads", "Threads"),
                    ("fit", "Fit"),
                    ("n", "Pairs"),
                    ("fastwarp_s", "FastWarp (s)"),
                    ("ants_s", "ANTs (s)"),
                    ("ratio", "ANTs / FastWarp"),
                    ("landmark_fastwarp", "Landmarks FastWarp (mm)"),
                    ("landmark_ants", "Landmarks ANTs (mm)"),
                    ("agreement_mean", "FastWarp - ANTs mean (mm)"),
                    ("agreement_max", "FastWarp - ANTs max (mm)"),
                ],
            ),
        ]
    if pst:
        lines += [
            "",
            "## Post-processing",
            "",
            "Seconds per step, median (quartiles), each including reading the inputs (read "
            "once, untimed, before both tools, so both find them cached) and writing the "
            "output; ANTs: `antsApplyTransforms --float 1`. `.nii`: uncompressed "
            "output; `.nii.gz`: FastWarp's fields gzip level 1 in parallel, ITK (ANTs' outputs "
            "and FastWarp's re-warped image) its default level on one thread. Size: median "
            "output MB, FastWarp / ANTs. ANTs / FastWarp: the median over pairs of the ratio. "
            "Difference (of the `.nii` outputs): median over pairs of the max field difference "
            "(mm) or of the relative RMS difference of the re-warped images.",
            "",
            _md_table(
                post_timing_table(pst),
                [
                    ("dataset", "Dataset"),
                    ("grid", "Grid"),
                    ("threads", "Threads"),
                    ("step", "Step"),
                    ("format", "Output"),
                    ("n", "Pairs"),
                    ("voxels", "Voxels"),
                    ("ants_s", "ANTs (s)"),
                    ("fastwarp_s", "FastWarp (s)"),
                    ("ratio", "ANTs / FastWarp"),
                    ("size", "Size (MB)"),
                    ("difference", "Difference"),
                ],
            ),
        ]
    machines = timing.machines({"run": reg, "linear": lin, "post": pst})
    if len(machines) > 1:
        lines += [
            "",
            "## Machines",
            "",
            "The timing records come from more than one machine or software version:",
            "",
            _md_table(
                machine_table(machines),
                [
                    ("records", "Records"),
                    ("gpu", "GPU"),
                    ("gpu_driver", "Driver name"),
                    ("gpu_backend", "Backend"),
                    ("cpu", "CPU"),
                    ("threads", "Threads"),
                    ("fastwarp", "fastwarp"),
                    ("ants", "ANTs"),
                ],
            ),
        ]
    return lines


def _environments(work: Path) -> dict[str, dict[str, Any]]:
    out = {}
    for command, name in COMMANDS:
        path = work / name
        if path.is_file():
            try:
                out[command] = json.loads(path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                continue
    return out


def report(*, work: Path, recipe_name: str, plot: bool = True, iso_mm: float | None = None) -> Path:
    """Write ``summary.md`` with the accuracy tables (when ``evaluate`` has run) and the timing
    tables, and the CSVs behind them (``paired.csv``, ``agreement_summary.csv``,
    ``structures.csv``, ``timings.csv``, ``linear.csv``, ``post.csv``; with matplotlib
    ``summary.png``) under ``<work>/results/<recipe>/``; returns the Markdown path. Timings are
    taken only from records of the recipe as it is defined now, grouped by registration grid
    (only ``iso_mm``, if given)."""
    folder = work / "results" / recipe_name
    pairs = _read_csv(folder / "pairs.csv")
    rec = recipes.recipe(recipe_name)
    reg = timing.registration_rows(work, recipe_name, rec)
    lin = linear.rows(work, recipe_name)
    pst = post.rows(work, recipe_name, rec)
    if iso_mm is not None:
        reg, lin, pst = ([r for r in x if r.get("iso_mm") == iso_mm] for x in (reg, lin, pst))
    if not (pairs or reg or lin or pst):
        raise RuntimeError(f"nothing to report in {work} for {recipe_name}; run (and evaluate)")
    folder.mkdir(parents=True, exist_ok=True)
    envs = _environments(work)
    env: dict[str, Any] = {}
    source = "no command (no environment file)"
    for command, name in COMMANDS:
        if command in envs:
            env, source = envs[command], f"the latest `{command}` (`{name}`)"
            break
    lines = [
        f"# FastWarp benchmark: recipe `{recipe_name}`",
        "",
        f"Machine and software of {source}:",
        "",
        *machine_lines(env),
        "",
        "Thread setting of each command (all cores unless `--threads` capped both engines; "
        "timings with different settings are not comparable, so the tables keep them apart):",
        "",
        *thread_lines(envs),
    ]
    if pairs:
        paired = paired_table(pairs)
        agreement = agreement_table(_read_csv(folder / "agreement.csv"))
        structures = per_structure_table(_read_csv(folder / "labels.csv"))
        _write_csv(folder / "paired.csv", paired)
        _write_csv(folder / "agreement_summary.csv", agreement)
        _write_csv(folder / "structures.csv", structures)
        lines += [
            "",
            "## FastWarp against ANTs, paired over the same pairs",
            "",
            "Difference = FastWarp minus ANTs; CI = 95 % percentile bootstrap of the mean "
            "difference; p = Wilcoxon signed-rank (pairs share subjects, so the p-values and CIs "
            "treat dependent pairs as independent).",
            "",
            _md_table(
                paired,
                [
                    ("dataset", "Dataset"),
                    ("metric", "Metric"),
                    ("better", "Better"),
                    ("n", "Pairs"),
                    ("mean_a", "FastWarp mean"),
                    ("sd_a", "SD"),
                    ("mean_b", "ANTs mean"),
                    ("sd_b", "SD"),
                    ("mean_diff", "Mean diff."),
                    ("ci_low", "CI low"),
                    ("ci_high", "CI high"),
                    ("wilcoxon_p", "p"),
                ],
            ),
            "",
            "## Agreement between the two engines",
            "",
            "Field differences (mm) inside the fixed brain mask; Dice between the label maps the "
            "two engines warp.",
            "",
            _md_table(
                agreement,
                [
                    ("dataset", "Dataset"),
                    ("metric", "Metric"),
                    ("n", "Pairs"),
                    ("mean", "Mean"),
                    ("sd", "SD"),
                    ("max", "Max"),
                ],
            ),
        ]
        small = [r for r in structures if r["group"] != "cortical"]
        if small:
            lines += [
                "",
                "## Mean Dice per structure (non-cortical)",
                "",
                _md_table(
                    small,
                    [
                        ("dataset", "Dataset"),
                        ("label", "Label"),
                        ("name", "Structure"),
                        ("dice_fastwarp_mean", "FastWarp"),
                        ("dice_ants_mean", "ANTs"),
                        ("dice_fastwarp_n", "Pairs"),
                    ],
                ),
            ]
    _write_csv(folder / "timings.csv", reg)
    _write_csv(folder / "linear.csv", lin)
    _write_csv(folder / "post.csv", pst)
    lines += _timing_sections(reg, lin, pst)
    if pairs and plot and _plot(pairs, folder / "summary.png"):
        lines += ["", "![per-pair results](summary.png)"]
    md = folder / "summary.md"
    md.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return md
