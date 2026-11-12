# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Write a demo clinical table for a group so the group analyses can be tried end to end.

The values are synthetic. Each hemisphere's improvement grows with how much of its
template-space VTA lies near a fixed motor STN target, plus seeded noise, so sweet-spot
mapping, fibre filtering and network mapping have a signal to find.

Usage:
    uv run python scripts/demo_clinical.py <group.json> <out.tsv> [--label LABEL] [--seed N]
"""

import argparse
import csv
import json
import random
import sys
from pathlib import Path

import nibabel as nib
import numpy as np

TARGET_MM = {"R": (12.6, -13.4, -5.9), "L": (-12.6, -13.4, -5.9)}
SIGMA_MM = 2.5
HEMI = {"R": "hemi-R", "L": "hemi-L"}


def vta_hit(path: Path, side: str) -> float | None:
    if not path.is_file():
        return None
    img = nib.load(path)
    data = np.asarray(img.dataobj)
    ijk = np.argwhere(data > 0.5)
    if ijk.size == 0:
        return 0.0
    xyz = nib.affines.apply_affine(img.affine, ijk)
    d2 = ((xyz - np.asarray(TARGET_MM[side])) ** 2).sum(axis=1)
    voxel_mm3 = float(abs(np.linalg.det(img.affine[:3, :3])))
    return float(np.exp(-d2 / (2.0 * SIGMA_MM**2)).sum() * voxel_mm3)


def find_vta(subject_dir: Path, space: str, label: str, side: str) -> Path:
    folder = subject_dir / "stimulations" / space / label
    name = f"{subject_dir.name}_sim-binary_model-simbio_{HEMI[side]}.nii"
    return folder / name


def clamp(v: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, v))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("group", type=Path)
    ap.add_argument("out", type=Path)
    ap.add_argument("--label", help="stimulation label (default: the group's)")
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()

    record = json.loads(args.group.read_text(encoding="utf-8"))
    label = args.label or (record.get("stimulation") or {}).get("label")
    if not label:
        print("the group has no stimulation label; pass --label", file=sys.stderr)
        return 2
    if not record.get("patients"):
        print("the group has no patients", file=sys.stderr)
        return 2
    space = record.get("space") or "MNI152NLin2009bAsym"
    roots = {d["id"]: Path(d["derivatives_root"]) for d in record["datasets"]}
    rng = random.Random(args.seed)

    rows = []
    for patient in record["patients"]:
        sub = patient["subject"]
        subject_dir = roots[patient["dataset"]] / sub
        hits = {
            s: vta_hit(find_vta(subject_dir, space, label, s), s) for s in ("R", "L")
        }
        if all(h is None for h in hits.values()):
            print(f"{sub}: no {label} VTA found, row left empty", file=sys.stderr)
        side_gain = {}
        for s, h in hits.items():
            if h is None:
                side_gain[s] = None
                continue
            side_gain[s] = clamp(15.0 + 1.6 * h + rng.gauss(0.0, 8.0), 0.0, 95.0)
        gains = [g for g in side_gain.values() if g is not None]
        mean_gain = sum(gains) / len(gains) if gains else None
        baseline = round(rng.uniform(28.0, 55.0))
        if mean_gain is None:
            followup = None
            improvement = None
        else:
            followup = round(baseline * (1.0 - mean_gain / 100.0))
            improvement = round((baseline - followup) / baseline * 100.0, 1)
        rows.append(
            {
                "participant_id": sub,
                "UPDRS3_baseline": baseline,
                "UPDRS3_followup": followup,
                "UPDRS3_improvement_pct": improvement,
                "responder": None if improvement is None else int(improvement >= 30.0),
                "bradykinesia_improvement_pct_R": None
                if side_gain["R"] is None
                else round(side_gain["R"], 1),
                "bradykinesia_improvement_pct_L": None
                if side_gain["L"] is None
                else round(side_gain["L"], 1),
                "age_years": rng.randint(48, 74),
                "disease_duration_years": rng.randint(6, 19),
                "LEDD_reduction_pct": round(
                    clamp(
                        (improvement or 30.0) * 0.8 + rng.gauss(0.0, 12.0), -10.0, 90.0
                    ),
                    1,
                ),
            }
        )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8", newline="") as fh:
        w = csv.DictWriter(
            fh, fieldnames=list(rows[0]), delimiter="\t", lineterminator="\n"
        )
        w.writeheader()
        for r in rows:
            w.writerow({k: "n/a" if v is None else v for k, v in r.items()})
    print(f"wrote {len(rows)} rows to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
