# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Command line:
``python -m benchmarks.accuracy {list,pin,fetch,plan,run,linear,post,evaluate,report}``.

The steps and the time they take are in ``docs/benchmarks.md``.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from . import datasets, recipes


def _keys(args: argparse.Namespace) -> list[str]:
    if args.dataset:
        for k in args.dataset:
            datasets.get(k)
        return list(args.dataset)
    return datasets.default_keys()


def _linear_keys(args: argparse.Namespace) -> list[str]:
    from .linear import DEFAULT_KEYS

    return _keys(args) if args.dataset else list(DEFAULT_KEYS)


def _add_selection(p: argparse.ArgumentParser) -> None:
    p.add_argument(
        "--dataset",
        action="append",
        help="dataset key (repeatable); default: all datasets of the default run",
    )
    p.add_argument("--recipe", default="cohort", choices=sorted(recipes.RECIPES))
    g = p.add_mutually_exclusive_group()
    g.add_argument("--pairs", type=int, help="number of pairs per dataset (default per dataset)")
    g.add_argument("--quick", action="store_true", help="one pair per dataset")


def _add_work(p: argparse.ArgumentParser, *, data: bool = True) -> None:
    if data:
        p.add_argument("--data", type=Path, required=True, help="data folder (outside the repo)")
    p.add_argument("--work", type=Path, required=True, help="output folder (outside the repo)")


def _cmd_list(args: argparse.Namespace) -> int:
    for key, d in datasets.registry().items():
        unpinned = sum(1 for f in d.downloads if not f.sha256)
        print(
            f"{key:12s} {'default' if d.in_default_run else 'optional':8s} {d.protocol:9s} "
            f"{len(d.subjects):3d} subjects  {d.download_bytes / 1e9:5.2f} GB  "
            f"pairs {d.default_pairs} (quick {d.quick_pairs})"
            + (f"  {unpinned} UNPINNED" if unpinned else "")
        )
        print(f"{'':12s} {d.title}; {d.licence}")
    return 0


def _cmd_pin(args: argparse.Namespace) -> int:
    from .fetch import HashMismatchError, stream_sha256

    for key in _keys(args):
        for d in datasets.get(key).downloads:
            if d.sha256 and not args.all:
                continue
            try:
                sha, size = stream_sha256(d.url, size=d.size)
            except HashMismatchError as e:
                row = {"dataset": key, "path": d.path, "url": d.url, "pinned": "DIFFERS"}
                print(json.dumps({**row, "error": str(e)}))
                continue
            status = "" if not d.sha256 else ("matches" if d.sha256 == sha else "DIFFERS")
            print(
                json.dumps(
                    {
                        "dataset": key,
                        "path": d.path,
                        "url": d.url,
                        "size": size,
                        "sha256": sha,
                        "pinned": status,
                    }
                )
            )
    return 0


def _cmd_fetch(args: argparse.Namespace) -> int:
    from .fetch import fetch_dataset

    for key in _keys(args):
        root = fetch_dataset(datasets.get(key), args.data, keep_archives=args.keep_archives)
        print(f"{key}: ready in {root}")
    return 0


def _cmd_plan(args: argparse.Namespace) -> int:
    from .fetch import check_outside_repo
    from .run import plan

    p = plan(
        work=check_outside_repo(args.work),
        keys=_keys(args),
        recipe_name=args.recipe,
        pairs=args.pairs,
        quick=args.quick,
        iso_mm=args.iso_mm,
        linear_keys=_linear_keys(args),
    )
    for step, title in (("datasets", "run + evaluate"), ("post", "post"), ("linear", "linear")):
        for key, d in p[step].items():
            print(
                f"{title} {key}: {d['pairs']} pairs, to run {d['to_run']}, "
                f"about {d['estimate_s'] / 3600:.1f} h"
            )
    secs = ", ".join(f"{k} {v:.0f} s" for k, v in p["seconds_per"].items())
    print(f"total about {p['total_s'] / 3600:.1f} h (seconds per pair and engine: {secs})")
    return 0


def _cmd_run(args: argparse.Namespace) -> int:
    from .fetch import check_outside_repo
    from .run import run

    engines = tuple(args.engine) if args.engine else ("fastwarp", "ants")
    rows = run(
        data=check_outside_repo(args.data),
        work=check_outside_repo(args.work),
        keys=_keys(args),
        recipe_name=args.recipe,
        pairs=args.pairs,
        quick=args.quick,
        engines=engines,
        iso_mm=args.iso_mm,
        threads=args.threads,
        ants_dir=args.ants_dir,
        adapter=args.adapter,
        retry_failed=args.retry_failed,
        ants_timeout_s=args.ants_timeout_h * 3600.0,
    )
    bad = [r for r in rows if r["status"] != "ok"]
    for r in bad:
        print(f"not ok: {r['dataset']}/{r['pair']} {r['engine']}: {r['status']}")
    return 1 if bad else 0


def _cmd_linear(args: argparse.Namespace) -> int:
    from .fetch import check_outside_repo
    from .linear import run_linear

    rows = run_linear(
        data=check_outside_repo(args.data),
        work=check_outside_repo(args.work),
        keys=_linear_keys(args),
        recipe_name=args.recipe,
        pairs=args.pairs,
        quick=args.quick,
        engines=tuple(args.engine) if args.engine else ("fastwarp", "ants"),
        iso_mm=args.iso_mm,
        threads=args.threads,
        ants_dir=args.ants_dir,
        adapter=args.adapter,
        retry_failed=args.retry_failed,
        ants_timeout_s=args.ants_timeout_h * 3600.0,
    )
    bad = [r for r in rows if r["status"] != "ok"]
    for r in bad:
        print(f"not ok: {r['dataset']}/{r['pair']} {r['engine']}: {r['status']}")
    return 1 if bad else 0


def _cmd_post(args: argparse.Namespace) -> int:
    from .fetch import check_outside_repo
    from .post import run_post

    rows = run_post(
        data=check_outside_repo(args.data),
        work=check_outside_repo(args.work),
        keys=_keys(args),
        recipe_name=args.recipe,
        pairs=args.pairs,
        quick=args.quick,
        iso_mm=args.iso_mm,
        threads=args.threads,
        ants_dir=args.ants_dir,
        retry_failed=args.retry_failed,
        ants_timeout_s=args.ants_timeout_h * 3600.0,
        gzip=args.gzip,
    )
    bad = [r for r in rows if r["status"] != "ok"]
    for r in bad:
        print(f"not ok: {r['dataset']}/{r['pair']}: {r['status'] or 'no registration'}")
    return 1 if bad else 0


def _cmd_evaluate(args: argparse.Namespace) -> int:
    from .evaluate import evaluate
    from .fetch import check_outside_repo
    from .run import RunLock, itk_threads

    with RunLock(check_outside_repo(args.work)), itk_threads(args.threads):
        out = evaluate(
            data=check_outside_repo(args.data),
            work=check_outside_repo(args.work),
            keys=_keys(args),
            recipe_name=args.recipe,
            pairs=args.pairs,
            quick=args.quick,
            surface=not args.no_surface,
            force=args.force,
        )
    print(f"tables in {out}")
    return 0


def _cmd_report(args: argparse.Namespace) -> int:
    from .fetch import check_outside_repo
    from .report import report

    md = report(
        work=check_outside_repo(args.work),
        recipe_name=args.recipe,
        plot=not args.no_plot,
        iso_mm=args.iso_mm,
    )
    print(md.read_text(encoding="utf-8"))
    print(f"written to {md}")
    return 0


def main(argv: list[str] | None = None) -> int:
    """Entry point."""
    parser = argparse.ArgumentParser(prog="python -m benchmarks.accuracy", description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("list", help="list the datasets").set_defaults(func=_cmd_list)

    p = sub.add_parser("pin", help="stream unpinned files through SHA-256 (nothing is stored)")
    p.add_argument("--dataset", action="append")
    p.add_argument("--all", action="store_true", help="also re-check files that have a pin")
    p.set_defaults(func=_cmd_pin)

    p = sub.add_parser("fetch", help="download, verify and extract the datasets")
    p.add_argument("--dataset", action="append")
    p.add_argument("--data", type=Path, required=True, help="data folder (outside the repo)")
    p.add_argument("--keep-archives", action="store_true")
    p.set_defaults(func=_cmd_fetch)

    p = sub.add_parser("plan", help="pairs left to run and the estimated time")
    _add_selection(p)
    _add_work(p, data=False)
    p.add_argument("--iso-mm", type=float, default=1.0, help="registration grid (default 1.0)")
    p.set_defaults(func=_cmd_plan)

    p = sub.add_parser("run", help="register the pairs with FastWarp and ANTs (resumable)")
    _add_selection(p)
    _add_work(p)
    p.add_argument(
        "--engine",
        action="append",
        choices=("fastwarp", "ants"),
        help="run only this engine (repeatable; default both)",
    )
    p.add_argument("--iso-mm", type=float, default=1.0, help="registration grid (default 1.0)")
    p.add_argument(
        "--threads",
        type=int,
        help="cap the CPU threads of ANTs and of FastWarp's CPU work (default: all cores)",
    )
    p.add_argument(
        "--ants-dir",
        type=Path,
        help="folder with an antsRegistration program to use instead of ANTsPy's",
    )
    p.add_argument("--adapter", help="GPU adapter name substring")
    p.add_argument("--retry-failed", action="store_true")
    p.add_argument("--ants-timeout-h", type=float, default=4.0)
    p.set_defaults(func=_cmd_run)

    p = sub.add_parser("linear", help="time linear_session fits against antsRegistration")
    _add_selection(p)
    _add_work(p)
    p.add_argument(
        "--engine",
        action="append",
        choices=("fastwarp", "ants"),
        help="run only this engine (repeatable; default both)",
    )
    p.add_argument("--iso-mm", type=float, default=1.0, help="registration grid (default 1.0)")
    p.add_argument(
        "--threads",
        type=int,
        help="cap the CPU threads of ANTs and of FastWarp's CPU work (default: all cores)",
    )
    p.add_argument("--ants-dir", type=Path, help="folder with the ANTs programs to use")
    p.add_argument("--adapter", help="GPU adapter name substring")
    p.add_argument("--retry-failed", action="store_true")
    p.add_argument("--ants-timeout-h", type=float, default=1.0)
    p.set_defaults(func=_cmd_linear)

    p = sub.add_parser(
        "post", help="time compose_field / apply_transforms against antsApplyTransforms"
    )
    _add_selection(p)
    _add_work(p)
    p.add_argument("--iso-mm", type=float, default=1.0, help="registration grid (default 1.0)")
    p.add_argument(
        "--threads",
        type=int,
        help="cap the CPU threads of ANTs and of FastWarp's CPU work (default: all cores)",
    )
    p.add_argument(
        "--gzip", action="store_true", help="also time writing .nii.gz outputs (doubles the time)"
    )
    p.add_argument("--ants-dir", type=Path, help="folder with the ANTs programs to use")
    p.add_argument("--retry-failed", action="store_true")
    p.add_argument("--ants-timeout-h", type=float, default=1.0)
    p.set_defaults(func=_cmd_post)

    p = sub.add_parser("evaluate", help="metrics of the finished pairs, CSV tables")
    _add_selection(p)
    _add_work(p)
    p.add_argument("--threads", type=int, help="cap the ITK threads (default: all cores)")
    p.add_argument("--no-surface", action="store_true", help="skip HD95 / ASSD (faster)")
    p.add_argument("--force", action="store_true", help="re-evaluate cached pairs")
    p.set_defaults(func=_cmd_evaluate)

    p = sub.add_parser("report", help="summary tables (accuracy and timings) and plot")
    p.add_argument("--recipe", default="cohort", choices=sorted(recipes.RECIPES))
    _add_work(p, data=False)
    p.add_argument("--no-plot", action="store_true")
    p.add_argument("--iso-mm", type=float, help="only timings on this grid (default: all, grouped)")
    p.set_defaults(func=_cmd_report)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
