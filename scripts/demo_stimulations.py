# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Write a set of demo stimulations into subjects so the GUI stimulation editor can load them.

Usage:
    uv run python scripts/demo_stimulations.py <dataset_root> [subject ...]
        [--dbsloc target/release/dbsloc.exe] [--output-dir DIR] [--json-dir DIR] [--dry-run]
"""

import argparse
import json
import subprocess
import sys
import tempfile
from collections.abc import Callable
from pathlib import Path

from dbsloc.electrodes import get_elspec

type Side = dict[str, object]
type Builder = Callable[[tuple[tuple[int, ...], ...]], Side | None]

SIDES = ("right", "left")


def split(contacts: tuple[int, ...], polarity: str) -> list[dict[str, object]]:
    n = len(contacts)
    shares = [round(100.0 / n, 2)] * n
    shares[-1] = round(100.0 - sum(shares[:-1]), 2)
    return [
        {"index": c, "polarity": polarity, "percent": p, "impedance_kohm": 1.0}
        for c, p in zip(contacts, shares, strict=True)
    ]


def source(
    index: int,
    mode: str,
    amplitude: float,
    contacts: list[dict[str, object]],
    case_anode: bool = True,
    pulse_width_us: float = 60.0,
) -> dict[str, object]:
    return {
        "index": index,
        "mode": mode,
        "amplitude": amplitude,
        "pulse_width_us": pulse_width_us,
        "frequency_hz": 130.0,
        "case": {"polarity": "anode", "percent": 100.0}
        if case_anode
        else {"polarity": "off", "percent": 0.0},
        "contacts": contacts,
    }


def side(*sources: dict[str, object]) -> Side:
    return {"active_source": 1, "sources": list(sources)}


def level(levels: tuple[tuple[int, ...], ...], i: int) -> tuple[int, ...]:
    return levels[min(i, len(levels) - 1)]


def single_contact(levels: tuple[tuple[int, ...], ...]) -> int:
    ring = [lv for lv in levels if len(lv) == 1]
    return ring[1][0] if len(ring) == len(levels) else ring[0][0]


def mono_ring_current(levels):
    return side(source(1, "current", 3.0, split(level(levels, 1), "cathode")))


def mono_ring_voltage(levels):
    return side(source(1, "voltage", 2.5, split(level(levels, 2), "cathode")))


def bipolar(levels):
    contacts = split(level(levels, 1), "cathode") + split(level(levels, 2), "anode")
    return side(source(1, "current", 3.0, contacts, case_anode=False))


def directional_segment(levels):
    lv = level(levels, 1)
    if len(lv) < 2:
        return None
    return side(
        source(1, "current", 2.0, split(lv[:1], "cathode"), pulse_width_us=30.0)
    )


def two_sources(levels):
    return side(
        source(1, "current", 2.0, split(level(levels, 1), "cathode")),
        source(2, "current", 1.5, split(level(levels, 2), "cathode")),
    )


def high_amplitude(levels):
    contacts = split(level(levels, 1) + level(levels, 2), "cathode")
    return side(source(1, "current", 5.0, contacts, pulse_width_us=90.0))


def analytical_voltage(levels):
    return side(source(1, "voltage", 3.0, split((single_contact(levels),), "cathode")))


def analytical_current(levels):
    return side(source(1, "current", 3.0, split((single_contact(levels),), "cathode")))


def idle(levels):
    return side()


EFIELD = {"threshold_v_per_mm": 0.2}
VIM = {
    "threshold_v_per_mm": 0.165,
    "conductivity": {"mode": "homogeneous", "homogeneous_s_per_m": 0.1},
}
SUM = {"threshold_v_per_mm": 0.2, "source_combination": "sum"}

MONTAGES: list[tuple[str, str, dict[str, object], Builder, Builder, str]] = [
    (
        "demo-mono-L2-3mA",
        "efield",
        EFIELD,
        mono_ring_current,
        mono_ring_current,
        "E-field, monopolar level 2, 3 mA, case anode, both leads",
    ),
    (
        "demo-mono-L3-2.5V",
        "efield",
        EFIELD,
        mono_ring_voltage,
        mono_ring_voltage,
        "E-field, monopolar level 3, 2.5 V (voltage mode), both leads",
    ),
    (
        "demo-bipolar-L2-L3",
        "efield",
        EFIELD,
        bipolar,
        bipolar,
        "E-field, bipolar: level 2 cathode, level 3 anode, case off, 3 mA",
    ),
    (
        "demo-segment-2mA",
        "efield",
        EFIELD,
        directional_segment,
        directional_segment,
        "E-field, one segment of level 2, 2 mA, 30 us (directional leads only)",
    ),
    (
        "demo-two-sources-sum",
        "efield",
        SUM,
        two_sources,
        two_sources,
        "E-field, source 1 level 2 at 2 mA + source 2 level 3 at 1.5 mA, fields summed",
    ),
    (
        "demo-right-only-5mA",
        "efield",
        EFIELD,
        high_amplitude,
        idle,
        "E-field, right lead only, levels 2+3 at 5 mA, 90 us; left lead off",
    ),
    (
        "demo-homogeneous-vim",
        "efield",
        VIM,
        mono_ring_current,
        mono_ring_current,
        "E-field, homogeneous 0.1 S/m, VIM threshold 0.165 V/mm, 3 mA",
    ),
    (
        "demo-kuncel-3V",
        "kuncel",
        EFIELD,
        analytical_voltage,
        analytical_voltage,
        "Kuncel 2008, one ring contact, 3 V",
    ),
    (
        "demo-maedler-3V",
        "maedler",
        EFIELD,
        analytical_voltage,
        analytical_voltage,
        "Maedler 2012, one ring contact, 3 V, 1 kOhm",
    ),
    (
        "demo-dembek-3mA",
        "dembek",
        EFIELD,
        analytical_current,
        analytical_current,
        "Dembek 2017, one ring contact, 3 mA",
    ),
]


def run_dbsloc(exe: str, args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run([exe, *args], capture_output=True, text=True, check=False)


def subject_levels(
    exe: str, root: Path, subject: str, extra: list[str]
) -> dict[str, tuple[tuple[int, ...], ...]]:
    p = run_dbsloc(exe, ["reco", "show", str(root), subject, *extra])
    if p.returncode != 0:
        raise RuntimeError(p.stderr.strip() or p.stdout.strip())
    out: dict[str, tuple[tuple[int, ...], ...]] = {}
    for s in json.loads(p.stdout)["sides"]:
        name = "right" if s["label"].lower().startswith("right") else "left"
        out[name] = tuple(tuple(lv) for lv in get_elspec(s["elmodel"]).etageidx)
    return out


def subjects_of(root: Path, output_dir: Path | None) -> list[str]:
    deriv = output_dir or root / "derivatives" / "leaddbs"
    return sorted(
        d.name.removeprefix("sub-")
        for d in deriv.glob("sub-*")
        if (d / "reconstruction").is_dir()
    )


def build(
    label: str,
    subject: str,
    model: str,
    settings: dict[str, object],
    sides: dict[str, Side],
) -> dict[str, object]:
    return {
        "schema": "dbsloc-stimulation",
        "schema_version": 1,
        "label": label,
        "subject": subject,
        "model": model,
        "model_settings": settings,
        "sides": sides,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("dataset_root", type=Path)
    ap.add_argument("subjects", nargs="*")
    ap.add_argument(
        "--dbsloc",
        default="target/release/dbsloc.exe"
        if sys.platform == "win32"
        else "target/release/dbsloc",
    )
    ap.add_argument("--output-dir", type=Path)
    ap.add_argument("--json-dir", type=Path)
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()
    a.dbsloc = str(Path(a.dbsloc).resolve())

    extra = ["--output-dir", str(a.output_dir)] if a.output_dir else []
    subjects = [s.removeprefix("sub-") for s in a.subjects] or subjects_of(
        a.dataset_root, a.output_dir
    )
    json_dir = a.json_dir or Path(tempfile.mkdtemp(prefix="dbsloc-demo-stim-"))
    failures = 0
    for subject in subjects:
        try:
            levels = subject_levels(a.dbsloc, a.dataset_root, subject, extra)
        except RuntimeError as e:
            print(f"sub-{subject}: skipped: {e}")
            continue
        for label, model, settings, right, left, text in MONTAGES:
            builders = {"right": right, "left": left}
            sides = {
                name: builders[name](levels[name]) if name in levels else idle(())
                for name in SIDES
            }
            if any(v is None for v in sides.values()):
                print(f"sub-{subject}: {label}: skipped (needs a directional lead)")
                continue
            path = json_dir / f"sub-{subject}" / f"{label}.json"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                json.dumps(build(label, subject, model, settings, sides), indent=2)
                + "\n"
            )
            if a.dry_run:
                print(f"sub-{subject}: {label}: written {path}")
                continue
            p = run_dbsloc(
                a.dbsloc,
                [
                    "stim",
                    "set",
                    str(a.dataset_root),
                    subject,
                    label,
                    "--from",
                    str(path),
                    *extra,
                ],
            )
            if p.returncode == 0:
                warn = [ln for ln in p.stderr.splitlines() if "warning" in ln]
                print(
                    f"sub-{subject}: {label}: ok ({text})"
                    + "".join(f"\n    {w}" for w in warn)
                )
            else:
                failures += 1
                print(
                    f"sub-{subject}: {label}: FAILED\n    "
                    + p.stderr.strip().replace("\n", "\n    ")
                )
    print(f"JSON files: {json_dir}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
