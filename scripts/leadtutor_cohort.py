# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
Lead-Tutor cohort harness: run the pipeline over the 10 validation subjects, score
reconstructions against the Lead-Tutor expert markers, compare runs, read per-stage
timings and check output determinism.

The expert markers are **not** redistributed with Wyss DBS Localizer: they derive from the Lead-Tutor
server's error vectors and stay outside the repository. Pass them by path; the expected
shape of ``--experts`` is::

    {"subjects": {"<sid>": {
        "leftElectrode":  {"head": [x, y, z], "tail": [x, y, z]},
        "rightElectrode": {"head": [x, y, z], "tail": [x, y, z]}}}}

A bare mapping ``{"<sid>": {...}}`` (no ``subjects`` wrapper) is accepted too. Extra keys
such as ``rotation_error_deg`` are ignored. ``leftElectrode`` is the +x lead.

Scores are Euclidean marker distances in MNI millimetres, per lead, as half the head
distance plus half the tail distance; a run's ``scores.json`` maps each subject to
``[+x lead, -x lead]``. The D9 rule passes when the candidate mean is at most the
reference mean plus 0.05 mm, no subject's mean is more than 0.5 mm worse and every
subject scored in the reference is scored in the candidate; ``compare`` exits 1 when it fails.

Sub-commands::

    run          run ``dbsloc run`` serially over the subjects (one ANTs process at a time)
    score        score a run root against the expert file, write <run-root>/scores.json
    compare      per-subject tables, means and the D9 rule for two run roots / scores files
    timings      per-stage CSV + Markdown table from the logs and the normalization records
    determinism  sha256 of the *_mode-image_xfm.h5 transforms and the *_desc-preproc_*.nii.gz images, two roots
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Callable, Iterable, Mapping
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

DEFAULT_SUBJECTS = (
    "15454",
    "29781",
    "33544",
    "39468",
    "57245",
    "78754",
    "80206",
    "84257",
    "93127",
    "76325",
)

DERIV = Path("derivatives") / "leaddbs"
LEAD_KEYS = ("leftElectrode", "rightElectrode")
ANTS_PROCESS_NAMES = (
    "antsRegistration.exe",
    "N4BiasFieldCorrection.exe",
    "antsRegistration",
    "N4BiasFieldCorrection",
)


def is_under(path: Path, base: Path) -> bool:
    try:
        return path.resolve().is_relative_to(base.resolve())
    except OSError, ValueError:
        return False


def temp_roots() -> list[Path]:
    roots: list[Path] = []
    for var in ("TEMP", "TMP", "TMPDIR"):
        value = os.environ.get(var, "").strip()
        if value:
            roots.append(Path(value))
    default = tempfile.gettempdir()
    if default:
        roots.append(Path(default))
    return roots


def assert_run_root_outside_temp(run_root: Path) -> None:
    for root in temp_roots():
        if is_under(run_root, root):
            raise SystemExit(
                f"error: --run-root {run_root} is under the temporary directory {root}; "
                "Storage Sense deletes old-mtime files there. Choose a run root outside it."
            )


def subject_dir(run_root: Path, sid: str) -> Path:
    return run_root / DERIV / f"sub-{sid}"


def find_reconstruction(run_root: Path, sid: str) -> Path | None:
    hits = sorted(
        (subject_dir(run_root, sid) / "reconstruction").glob(
            f"sub-{sid}_desc-reconstruction.json"
        )
    )
    return hits[-1] if hits else None


def load_experts(path: Path) -> dict[str, dict]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise SystemExit(f"error: {path} is not a JSON object")
    subjects = payload.get("subjects", payload)
    if not isinstance(subjects, dict):
        raise SystemExit(
            f"error: {path}: 'subjects' must be an object mapping subject ids to "
            "marker objects, not a " + type(subjects).__name__
        )
    return {
        str(sid): entry for sid, entry in subjects.items() if isinstance(entry, dict)
    }


def score_leads(reco: dict, expert: dict) -> list[float]:
    mni = reco["mni"]
    leads = sorted(mni.values(), key=lambda v: -float(v["markers"]["head"][0]))
    if len(leads) < 2:
        raise ValueError(f"reconstruction has {len(leads)} resolved lead(s), need 2")
    scores: list[float] = []
    for key, lead in zip(LEAD_KEYS, leads, strict=True):
        want = expert[key]
        head, tail = lead["markers"]["head"], lead["markers"]["tail"]
        scores.append(
            0.5 * (math.dist(head, want["head"]) + math.dist(tail, want["tail"]))
        )
    return scores


def score_subject(reco_path: Path, expert: dict) -> list[float]:
    return score_leads(json.loads(reco_path.read_text(encoding="utf-8")), expert)


def mean_of(values: list[float]) -> float:
    return sum(values) / len(values) if values else float("nan")


def score_run(
    run_root: Path, experts: dict[str, dict]
) -> dict[str, list[float] | None]:
    scores: dict[str, list[float] | None] = {}
    for sid in sorted(experts):
        reco = find_reconstruction(run_root, sid)
        if reco is None:
            scores[sid] = None
            continue
        try:
            scores[sid] = score_subject(reco, experts[sid])
        except (KeyError, ValueError) as exc:
            print(f"warning: sub-{sid}: {exc}", file=sys.stderr)
            scores[sid] = None
    return scores


def load_scores(path: Path) -> dict[str, list[float] | None]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise SystemExit(f"error: {path} is not a scores JSON object")
    scores: dict[str, list[float] | None] = {}
    for sid, v in payload.items():
        if v is None:
            scores[str(sid)] = None
        elif isinstance(v, (list, tuple)) and len(v) == 2:
            scores[str(sid)] = [float(v[0]), float(v[1])]
        else:
            raise SystemExit(
                f"error: {path}: sub-{sid} must be null or a pair of lead errors"
            )
    return scores


def _all_leads(scores: dict[str, list[float] | None]) -> list[float]:
    return [v for pair in scores.values() if pair for v in pair]


def render_score_table(scores: dict[str, list[float] | None]) -> str:
    lines = [f"{'subject':<10} {'+x':>8} {'-x':>8} {'mean':>8}"]
    for sid in sorted(scores):
        pair = scores[sid]
        if pair is None:
            lines.append(f"{'sub-' + sid:<10} {'--':>8} {'--':>8} {'--':>8}")
            continue
        lines.append(
            f"{'sub-' + sid:<10} {pair[0]:>8.3f} {pair[1]:>8.3f} {mean_of(pair):>8.3f}"
        )
    leads = _all_leads(scores)
    lines.append(
        f"{'mean':<10} {'':>8} {'':>8} {mean_of(leads):>8.3f}  over {len(leads)} leads"
    )
    return "\n".join(lines)


def d9_rule(
    reference: Mapping[str, list[float] | None],
    candidate: Mapping[str, list[float] | None],
    mean_margin: float = 0.05,
    subject_margin: float = 0.5,
) -> dict:
    scored_ref = {sid: v for sid, v in reference.items() if v is not None}
    scored_cand = {sid: v for sid, v in candidate.items() if v is not None}
    missing = sorted(set(scored_ref) - set(scored_cand))
    common = sorted(set(scored_ref) & set(scored_cand))
    ref_leads = [v for sid in common for v in scored_ref[sid]]
    cand_leads = [v for sid in common for v in scored_cand[sid]]
    mean_ref = mean_of(ref_leads)
    mean_cand = mean_of(cand_leads)
    subject_deltas = {
        sid: mean_of(scored_cand[sid]) - mean_of(scored_ref[sid]) for sid in common
    }
    worst_sid = max(subject_deltas, key=lambda sid: subject_deltas[sid], default=None)
    worst = subject_deltas[worst_sid] if worst_sid is not None else 0.0
    largest_lead = 0.0
    largest_at: str | None = None
    for sid in common:
        for lead, (r, c) in zip(
            ("+x", "-x"), zip(scored_ref[sid], scored_cand[sid], strict=True)
        ):
            if abs(c - r) > largest_lead:
                largest_lead = abs(c - r)
                largest_at = f"sub-{sid} {lead}"
    mean_ok = mean_cand <= mean_ref + mean_margin
    subject_ok = worst <= subject_margin
    coverage_ok = not missing
    return {
        "subjects": common,
        "missing": missing,
        "coverage_ok": coverage_ok,
        "mean_ref": mean_ref,
        "mean_cand": mean_cand,
        "mean_delta": mean_cand - mean_ref,
        "largest_lead_change": largest_lead,
        "largest_lead_at": largest_at,
        "worst_subject": worst_sid,
        "worst_subject_delta": worst,
        "mean_ok": mean_ok,
        "subject_ok": subject_ok,
        "pass": mean_ok and subject_ok and coverage_ok,
    }


def render_compare_table(
    reference: Mapping[str, list[float] | None],
    candidate: Mapping[str, list[float] | None],
) -> str:
    header = (
        f"{'subject':<10} "
        f"{'ref+x':>7} {'ref-x':>7} {'cand+x':>7} {'cand-x':>7} "
        f"{'d+x':>6} {'d-x':>6} {'dmean':>7}"
    )
    lines = [header]
    for sid in sorted(set(reference) | set(candidate)):
        r = reference.get(sid)
        c = candidate.get(sid)
        if r is None or c is None:
            lines.append(f"{'sub-' + sid:<10} missing on one side")
            continue
        dmean = mean_of(c) - mean_of(r)
        lines.append(
            f"{'sub-' + sid:<10} "
            f"{r[0]:>7.3f} {r[1]:>7.3f} {c[0]:>7.3f} {c[1]:>7.3f} "
            f"{c[0] - r[0]:>+6.3f} {c[1] - r[1]:>+6.3f} {dmean:>+7.3f}"
        )
    return "\n".join(lines)


def compare_scores(
    reference: Mapping[str, list[float] | None],
    candidate: Mapping[str, list[float] | None],
) -> str:
    verdict = d9_rule(reference, candidate)
    missing = verdict["missing"]
    lines = [
        render_compare_table(reference, candidate),
        "",
        "missing in candidate   : "
        + (", ".join(f"sub-{sid}" for sid in missing) if missing else "none"),
        f"reference mean : {verdict['mean_ref']:.3f} mm over {len(verdict['subjects'])} subjects",
        f"candidate mean : {verdict['mean_cand']:.3f} mm (delta {verdict['mean_delta']:+.3f})",
        f"largest per-lead change: {verdict['largest_lead_change']:.3f} mm"
        + (f" at {verdict['largest_lead_at']}" if verdict["largest_lead_at"] else ""),
        f"worst subject change   : {verdict['worst_subject_delta']:+.3f} mm"
        + (f" (sub-{verdict['worst_subject']})" if verdict["worst_subject"] else ""),
        (
            f"D9: {'PASS' if verdict['pass'] else 'FAIL'}"
            f"  (mean <= ref + 0.05: {'yes' if verdict['mean_ok'] else 'no'};"
            f" no subject worse by > 0.5: {'yes' if verdict['subject_ok'] else 'no'};"
            f" every reference subject scored: {'yes' if verdict['coverage_ok'] else 'no'})"
        ),
    ]
    return "\n".join(lines)


def parse_profile_log(text: str) -> dict | None:
    decoder = json.JSONDecoder()
    found: dict | None = None
    for i, ch in enumerate(text):
        if ch != "{":
            continue
        try:
            obj, _ = decoder.raw_decode(text[i:])
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict) and "stage_timings_sec" in obj:
            found = obj
    return found


def find_log(run_root: Path, sid: str) -> Path | None:
    direct = run_root / "logs" / f"sub-{sid}.log"
    if direct.is_file():
        return direct
    hits = sorted(
        p for p in run_root.glob("*.log") if sid in p.name and "driver" not in p.name
    )
    return hits[-1] if hits else None


def normalization_record(run_root: Path, sid: str) -> dict | None:
    record = (
        subject_dir(run_root, sid)
        / "normalization"
        / "log"
        / f"sub-{sid}_desc-normmethod.json"
    )
    try:
        return json.loads(record.read_text(encoding="utf-8"))
    except OSError, json.JSONDecodeError:
        return None


def level_seconds(record: dict, key: str) -> float | None:
    levels = record.get(key)
    if not isinstance(levels, list):
        return None
    values = [
        float(lv["seconds"])
        for lv in levels
        if isinstance(lv, dict) and "seconds" in lv
    ]
    return sum(values) if values else None


def render_markdown(title: str, headers: list[str], rows: list[list[str]]) -> str:
    lines = [
        f"## {title}",
        "",
        "| " + " | ".join(headers) + " |",
        "| " + " | ".join("---" for _ in headers) + " |",
    ]
    for row in rows:
        lines.append("| " + " | ".join(row) + " |")
    return "\n".join(lines)


def render_csv(table: str, headers: list[str], rows: list[list[str]]) -> str:
    lines = [f"table,{','.join(headers)}"]
    for row in rows:
        lines.append(f"{table},{','.join(row)}")
    return "\n".join(lines)


LEVEL_COLUMNS = (
    ("t2_coreg_levels_s", "reg_t2_coreg_levels"),
    ("rigid_levels_s", "reg_rigid_levels"),
    ("affine_levels_s", "reg_affine_levels"),
    ("syn_levels_s", "reg_syn_levels"),
)


def timings_tables(run_root: Path, subjects: list[str]) -> tuple[str, str]:
    stage_keys: list[str] = []
    stage_rows: list[list[str]] = []
    detail_keys: list[str] = []
    detail_rows: list[list[str]] = []
    for sid in subjects:
        profile: dict = {}
        log = find_log(run_root, sid)
        if log is not None:
            profile = (
                parse_profile_log(log.read_text(encoding="utf-8", errors="replace"))
                or {}
            )
        stage_timings = profile.get("stage_timings_sec", {})
        for key in stage_timings:
            if key not in stage_keys:
                stage_keys.append(key)
        record = normalization_record(run_root, sid) or {}
        reg = record.get("reg_timings", {})
        for key in reg:
            if key not in detail_keys:
                detail_keys.append(key)
        stage_rows.append(
            [f"sub-{sid}"]
            + [
                "" if k not in stage_timings else f"{float(stage_timings[k]):.3f}"
                for k in stage_keys
            ]
            + ["" if not profile else f"{float(profile.get('total_seconds', 0.0)):.3f}"]
        )
        detail = [f"sub-{sid}"]
        for key in detail_keys:
            value = reg.get(key)
            detail.append("" if value is None else f"{float(value):.3f}")
        for _, record_key in LEVEL_COLUMNS:
            value = level_seconds(record, record_key)
            detail.append("" if value is None else f"{value:.3f}")
        detail_rows.append(detail)
    stage_headers = ["subject", *stage_keys, "total_seconds"]
    detail_headers = ["subject", *detail_keys, *(col for col, _ in LEVEL_COLUMNS)]
    md = (
        render_markdown("Stage timings (s)", stage_headers, stage_rows)
        + "\n\n"
        + render_markdown("Normalize detail (s)", detail_headers, detail_rows)
        + "\n"
    )
    csv = (
        render_csv("stages", stage_headers, stage_rows)
        + "\n"
        + render_csv("normalize", detail_headers, detail_rows)
        + "\n"
    )
    return md, csv


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def artifact_hashes(run_root: Path, sid: str) -> dict[str, str]:
    base = subject_dir(run_root, sid)
    if not base.is_dir():
        return {}
    out: dict[str, str] = {}
    for path in sorted(base.rglob("*")):
        if not path.is_file():
            continue
        name = path.name
        if name.endswith("_mode-image_xfm.h5"):
            key = "xfm.h5"
        elif "_desc-preproc_" in name and name.endswith(".nii.gz"):
            key = "preproc.nii.gz"
        else:
            continue
        rel = path.relative_to(base).as_posix()
        out[f"{key}::{rel}"] = sha256_file(path)
    return out


def determinism_report(
    left_root: Path, right_root: Path, subjects: list[str]
) -> tuple[str, bool]:
    lines = [f"{'subject':<10} {'artifact':<44} {'match':>6}"]
    identical = True
    for sid in subjects:
        left = artifact_hashes(left_root, sid)
        right = artifact_hashes(right_root, sid)
        keys = sorted(set(left) | set(right))
        if not keys:
            lines.append(f"{'sub-' + sid:<10} {'(no artifacts)':<44} {'--':>6}")
            identical = False
            continue
        for key in keys:
            same = key in left and key in right and left[key] == right[key]
            identical &= same
            label = key if len(key) <= 44 else "…" + key[-43:]
            lines.append(
                f"{'sub-' + sid:<10} {label:<44} {'ok' if same else 'DIFF':>6}"
            )
    lines.append("")
    lines.append(f"determinism: {'identical' if identical else 'DIFFERENCES FOUND'}")
    return "\n".join(lines), identical


def dbsloc_command(explicit: Path | None = None) -> list[str]:
    if explicit is not None:
        if not explicit.is_file():
            raise SystemExit(f"error: --dbsloc {explicit} is not a file")
        return [str(explicit)]
    env = os.environ.get("DBSLOC_BIN", "").strip()
    if env and Path(env).is_file():
        return [env]
    name = "dbsloc.exe" if os.name == "nt" else "dbsloc"
    for profile in ("release", "debug"):
        candidate = REPO / "target" / profile / name
        if candidate.is_file():
            return [str(candidate)]
    exe = shutil.which("dbsloc")
    if exe:
        return [exe]
    raise SystemExit(
        "error: no dbsloc executable found: set DBSLOC_BIN, pass --dbsloc, "
        "or run `cargo build -p dbsloc-cli --release`"
    )


def parse_tasklist(text: str) -> list[str]:
    found: list[str] = []
    for line in text.splitlines():
        first = line.split(",", 1)[0].strip().strip('"')
        if first in ANTS_PROCESS_NAMES:
            found.append(first)
    return found


def running_ants_processes() -> list[str]:
    if os.name == "nt":
        try:
            out = subprocess.run(
                ["tasklist", "/FO", "CSV", "/NH"],
                capture_output=True,
                text=True,
                check=False,
            ).stdout
        except OSError:
            return []
        return parse_tasklist(out)
    found: list[str] = []
    for name in ("antsRegistration", "N4BiasFieldCorrection"):
        rc = subprocess.run(
            ["pgrep", "-x", name], capture_output=True, check=False
        ).returncode
        if rc == 0:
            found.append(name)
    return found


def build_run_argv(
    dbsloc: list[str],
    run_root: Path,
    sid: str,
    *,
    template: Path | None,
    isotropic_mm: str | None,
    stages: str | None,
    reg_backend: str | None,
    extra: list[str],
) -> list[str]:
    argv = [*dbsloc, "run", str(run_root), sid]
    argv += ["--recon-method", "trac_core_native"]
    argv += [
        "--headless",
        "--no-checkreg",
        "--no-scene3d",
        "--log",
        "plain",
        "--profile",
        "--overwrite-approved",
    ]
    if template is not None:
        argv += ["--template", str(template)]
    if isotropic_mm is not None:
        argv += ["--norm-isotropic-mm", isotropic_mm]
    if stages:
        argv += ["--stages", stages]
    if reg_backend:
        argv += ["--reg-backend", reg_backend]
    argv += extra
    return argv


def copy_tree(
    src: Path,
    dst: Path,
    ignore: Callable[[str, list[str]], Iterable[str]] | None = None,
) -> None:
    if dst.exists():
        return
    if not src.exists():
        return
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(src, dst, dirs_exist_ok=False, ignore=ignore)


def seed_subject(
    run_root: Path,
    sid: str,
    data_root: Path | None,
    seed_from: Path | None,
    seed_exclude: set[str] | None = None,
) -> None:
    source = data_root or seed_from
    if source is None:
        raise SystemExit("error: --data-root or --seed-from is required")
    description = source / "dataset_description.json"
    if description.is_file() and not (run_root / "dataset_description.json").exists():
        run_root.mkdir(parents=True, exist_ok=True)
        shutil.copy2(description, run_root / "dataset_description.json")
    copy_tree(source / "rawdata" / f"sub-{sid}", run_root / "rawdata" / f"sub-{sid}")
    copy_tree(
        source / DERIV / f"sub-{sid}" / "prefs",
        run_root / DERIV / f"sub-{sid}" / "prefs",
    )
    if seed_from is not None:
        ignore = shutil.ignore_patterns(
            "*_ses-preop*space-anchorNative*", "*_ses-preop*from-T2w_to-anchorNative*"
        )
        skip = seed_exclude or set()
        for stage in ("preprocessing", "coregistration"):
            if stage in skip:
                continue
            copy_tree(
                seed_from / DERIV / f"sub-{sid}" / stage,
                run_root / DERIV / f"sub-{sid}" / stage,
                ignore=ignore,
            )


def run_environment(
    *,
    reg_backend: str | None,
    gpu_backend: str | None,
    ants_threads: int | None,
) -> dict[str, str]:
    env = dict(os.environ)
    env["DBSLOC_HEADLESS"] = "1"
    env["DBSLOC_OPEN_SCENE3D"] = "0"
    env["PYTHONIOENCODING"] = "utf-8"
    if reg_backend:
        env["DBSLOC_REG_BACKEND"] = reg_backend
    if gpu_backend:
        env["DBSLOC_GPU_BACKEND"] = gpu_backend
        env["FASTWARP_GPU_BACKEND"] = gpu_backend
    if ants_threads is not None:
        env["DBSLOC_ANTS_MAX_THREADS"] = str(ants_threads)
        env["ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS"] = str(ants_threads)
    return env


def subjects_from_arg(values: list[str] | None) -> list[str]:
    if not values:
        return list(DEFAULT_SUBJECTS)
    out: list[str] = []
    for value in values:
        out.extend(part for part in value.split(",") if part)
    return out


def cmd_run(args: argparse.Namespace) -> int:
    run_root: Path = args.run_root
    assert_run_root_outside_temp(run_root)
    busy = running_ants_processes()
    if busy:
        raise SystemExit(
            f"error: refusing to start while an ANTs process is running: {', '.join(busy)}"
        )
    subjects = subjects_from_arg(args.subjects)
    dbsloc = dbsloc_command(args.dbsloc)
    env = run_environment(
        reg_backend=args.reg_backend,
        gpu_backend=args.gpu_backend,
        ants_threads=args.ants_threads,
    )
    extra: list[str] = []
    for chunk in args.extra or []:
        extra += shlex.split(chunk)
    logs = run_root / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    failures: list[str] = []
    for sid in subjects:
        seed_subject(run_root, sid, args.data_root, args.seed_from, args.seed_exclude)
        argv = build_run_argv(
            dbsloc,
            run_root,
            sid,
            template=args.template,
            isotropic_mm=args.norm_isotropic_mm,
            stages=args.stages,
            reg_backend=args.reg_backend,
            extra=extra,
        )
        log_path = logs / f"sub-{sid}.log"
        print(f"[run] sub-{sid}: {' '.join(argv)}", flush=True)
        with log_path.open("w", encoding="utf-8") as log:
            rc = subprocess.run(
                argv, env=env, stdout=log, stderr=subprocess.STDOUT, check=False
            ).returncode
        if rc != 0:
            failures.append(sid)
            print(f"[run] sub-{sid}: FAILED rc={rc} (see {log_path})", flush=True)
        else:
            print(f"[run] sub-{sid}: ok", flush=True)
    if failures:
        print(
            f"[run] {len(failures)} subject(s) failed: {', '.join(failures)}",
            flush=True,
        )
        return 1
    print(f"[run] done: {len(subjects)} subject(s)", flush=True)
    return 0


def _scores_path(run_root: Path) -> Path:
    path = run_root / "scores.json"
    if not path.is_file():
        raise SystemExit(
            f"error: {path} not found; run `leadtutor_cohort.py score --run-root {run_root} "
            "--experts <file>` first, or pass --from-scores with two scores files"
        )
    return path


def cmd_score(args: argparse.Namespace) -> int:
    run_root: Path = args.run_root
    experts = load_experts(args.experts)
    scores = score_run(run_root, experts)
    print(render_score_table(scores))
    run_root.mkdir(parents=True, exist_ok=True)
    (run_root / "scores.json").write_text(
        json.dumps(scores, indent=1, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"wrote {run_root / 'scores.json'}")
    return 0


def cmd_compare(args: argparse.Namespace) -> int:
    if args.from_scores:
        reference = load_scores(args.reference)
        candidate = load_scores(args.candidate)
    else:
        reference = load_scores(_scores_path(args.reference))
        candidate = load_scores(_scores_path(args.candidate))
    print(compare_scores(reference, candidate))
    return 0 if d9_rule(reference, candidate)["pass"] else 1


def cmd_timings(args: argparse.Namespace) -> int:
    run_root: Path = args.run_root
    subjects = subjects_from_arg(args.subjects)
    markdown, csv = timings_tables(run_root, subjects)
    csv_path = args.csv or run_root / "timings.csv"
    md_path = args.md or run_root / "timings.md"
    csv_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)
    csv_path.write_text(csv, encoding="utf-8")
    md_path.write_text(markdown, encoding="utf-8")
    print(markdown)
    print(f"wrote {csv_path} and {md_path}")
    return 0


def cmd_determinism(args: argparse.Namespace) -> int:
    subjects = subjects_from_arg(args.subjects)
    report, identical = determinism_report(args.left, args.right, subjects)
    print(report)
    return 0 if identical else 1


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = parser.add_subparsers(dest="command", required=True)

    p_run = sub.add_parser("run", help="run dbsloc serially over the subjects")
    p_run.add_argument(
        "--data-root", type=Path, default=None, help="source BIDS dataset"
    )
    p_run.add_argument(
        "--run-root",
        type=Path,
        required=True,
        help="output root; must not be under %%TEMP%% / $TMPDIR",
    )
    p_run.add_argument("--template", type=Path, default=None, help="MNI template NIfTI")
    p_run.add_argument(
        "--subjects", nargs="+", default=None, help="subject ids (default: the 10)"
    )
    p_run.add_argument(
        "--norm-isotropic-mm", default=None, help="registration grid in mm, or 'full'"
    )
    p_run.add_argument("--reg-backend", default=None, help="auto | wgpu | ants")
    p_run.add_argument("--stages", default=None, help="comma-separated stage list")
    p_run.add_argument(
        "--seed-from", type=Path, default=None, help="run root to seed from"
    )
    p_run.add_argument(
        "--seed-exclude",
        nargs="+",
        default=None,
        help="stage dirs not to copy from --seed-from (preprocessing, coregistration)",
    )
    p_run.add_argument(
        "--extra",
        action="append",
        default=None,
        help="extra dbsloc run arguments; repeatable, each value shell-split",
    )
    p_run.add_argument(
        "--dbsloc", type=Path, default=None, help="path to the dbsloc executable"
    )
    p_run.add_argument("--gpu-backend", default=None, help="sets DBSLOC_GPU_BACKEND")
    p_run.add_argument(
        "--ants-threads", type=int, default=None, help="caps ITK threads"
    )
    p_run.set_defaults(func=cmd_run)

    p_score = sub.add_parser(
        "score", help="score a run root against the expert markers"
    )
    p_score.add_argument("--run-root", type=Path, required=True)
    p_score.add_argument(
        "--experts", type=Path, required=True, help="expert markers JSON"
    )
    p_score.set_defaults(func=cmd_score)

    p_cmp = sub.add_parser("compare", help="two runs / scores: tables, means, D9 rule")
    p_cmp.add_argument("reference", type=Path)
    p_cmp.add_argument("candidate", type=Path)
    p_cmp.add_argument(
        "--from-scores",
        action="store_true",
        help="the two paths are scores JSON files, not run roots",
    )
    p_cmp.set_defaults(func=cmd_compare)

    p_time = sub.add_parser(
        "timings", help="per-stage timing table from logs and records"
    )
    p_time.add_argument("--run-root", type=Path, required=True)
    p_time.add_argument(
        "--subjects", nargs="+", default=None, help="subject ids (default: the 10)"
    )
    p_time.add_argument("--csv", type=Path, default=None)
    p_time.add_argument("--md", type=Path, default=None)
    p_time.set_defaults(func=cmd_timings)

    p_det = sub.add_parser("determinism", help="sha256 of the registration artifacts")
    p_det.add_argument("left", type=Path)
    p_det.add_argument("right", type=Path)
    p_det.add_argument(
        "--subjects", nargs="+", default=None, help="subject ids (default: the 10)"
    )
    p_det.set_defaults(func=cmd_determinism)

    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
