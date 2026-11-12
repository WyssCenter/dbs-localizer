# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Parse `specs/09_electrode_models.md` table §6.1, emit `dbsloc/electrodes/data/catalogue.json`."""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "specs" / "09_electrode_models.md"
OUT = ROOT / "dbsloc" / "electrodes" / "data" / "catalogue.json"

MARKERS: dict[str, tuple[float, float]] = {
    "medtronic_b33005": (14.4, 2.3),
    "medtronic_b33015": (17.4, 2.3),
    "boston_vercise_directed": (11.0, 3.0),
    "boston_vercise_cartesia_hx": (20.0, 3.0),
    "boston_vercise_cartesia_x": (16.0, 3.0),
    "abbott_directed_05": (10.75, 1.5),
    "abbott_directed_15": (13.75, 1.5),
    "aleva_directstim_directed": (13.0, 3.0),
}

ARCH = {
    "medtronic_3389": "ring4",
    "medtronic_3387": "ring4",
    "medtronic_3391": "ring4",
    "medtronic_b33005": "m8d",
    "medtronic_b33015": "m8d",
    "boston_vercise": "ring8",
    "boston_vercise_directed": "b8d_tip",
    "boston_vercise_cartesia_hx": "c16_hx",
    "boston_vercise_cartesia_x": "c16_x",
    "abbott_activetip_2mm": "ring4_tip",
    "abbott_activetip_3mm": "ring4_tip",
    "abbott_directed_05": "a8d",
    "abbott_directed_15": "a8d",
    "aleva_directstim_directed": "a12d",
    "elaine_rat_electrode": "ring1",
    "fhc_wu_rat_electrode": "ring1",
    "smartflow_ngs-nc-06": "smart2",
    "sceneray_sr1242": "ring4",
}


def _parse_cs(cs_raw: str) -> float | list[float]:
    s = re.sub(r"[\u2020\u2021\xa7]", "", cs_raw)
    s = s.replace("**", "").strip()
    if not s or s == "—":
        return 0.0
    m = re.match(r"^(\d+\.?\d*)/(\d+\.?\d*)/(\d+\.?\d*)/(\d+\.?\d*)$", s)
    if m:
        return [
            float(m.group(1)),
            float(m.group(2)),
            float(m.group(3)),
            float(m.group(4)),
        ]
    m = re.match(r"^(\d+\.?\d*)/(\d+\.?\d*)/(\d+\.?\d*)$", s)
    if m:
        return [float(m.group(1)), float(m.group(2)), float(m.group(3))]
    m = re.match(r"^(\d+\.?\d*)/(\d+\.?\d*)×(\d+)$", s)
    if m:
        v, w, c = float(m.group(1)), float(m.group(2)), int(m.group(3))
        return [v] + [w] * c
    m = re.match(r"^(\d+\.?\d*)/(\d+\.?\d*)/(\d+\.?\d*)×(\d+)$", s)
    if m:
        a, b, c, n = (
            float(m.group(1)),
            float(m.group(2)),
            float(m.group(3)),
            int(m.group(4)),
        )
        return [a, b] + [c] * n
    return float(s)


def _parse_ods(s: str) -> float:
    s = s.replace("**", "").split("†")[0].strip()
    if not s or s in ("—", "–", "-"):
        return 0.0
    return float(s)


def _parse_table() -> list[dict[str, object]]:
    lines = SPEC.read_text(encoding="utf-8").splitlines()
    out: list[dict[str, object]] = []
    for L in lines:
        if not L.strip().startswith("|") or re.match(r"^\|\s*#", L):
            continue
        p = [c.strip() for c in L.split("|")][1:-1]
        if len(p) < 11:
            continue
        if p[0] in ("#", "") or p[0] == "—":
            continue
        num_m = re.match(r"^(\d+)$", p[0].split()[0] if p[0] else "")
        if not num_m:
            continue
        n_idx = int(num_m.group(1))
        display, matfname, _manu = p[1], p[2], p[3]
        if not matfname or matfname in ("matfname", "Display Name", "--------"):
            continue
        n_cont = int(p[4]) if p[4].isdigit() else 0
        ods = p[5]
        odsf = _parse_ods(ods)
        cl = _parse_ods(p[6]) if p[6] else 0.0
        cs = _parse_cs(p[7]) if p[7] else 0.0
        tl = _parse_ods(p[8]) if p[8] else 0.0
        tic, dirb, _dio = p[9], p[10], p[11] if len(p) > 11 else "N"
        is_dir = "Y" in dirb
        is_tic = "Y" in tic or ("**Y**" in tic) or (tic.strip().startswith("**Y"))
        is_dio = "Y" in _dio
        mpos, mlen = MARKERS.get(matfname, (None, None))
        arch = ARCH.get(
            matfname,
            f"seeg_{n_cont}" if n_cont > 1 else "ring1",
        )
        if matfname in ("boston_vercise_cartesia_hx", "boston_vercise_cartesia_x"):
            is_dio = True
        out.append(
            {
                "ix": n_idx,
                "elmodel": " ".join(display.split()),
                "matfname": matfname,
                "numContacts": n_cont,
                "lead_diameter": odsf,
                "contact_diameter": odsf,
                "tip_diameter": odsf if matfname != "smartflow_ngs-nc-06" else 0.2,
                "tip_length": tl,
                "tipiscontact": is_tic,
                "isdirected": is_dir,
                "forstimulation": True,
                "isdio_supported": is_dio,
                "contact_length": cl,
                "contact_spacing": cs,
                "markerpos": mpos,
                "markerlen": mlen,
                "archetype": arch,
            }
        )
    return out


class CatalogueError(Exception):
    pass


def build_rows() -> list[dict[str, object]]:
    rows = _parse_table()
    if len(rows) != 73:
        raise CatalogueError(f"got {len(rows)} rows, expected 73")
    smart = next(x for x in rows if x["matfname"] == "smartflow_ngs-nc-06")
    if isinstance(smart, dict) and "contact_diameter" in smart:
        smart["contact_diameter"] = 0.27
        smart["tip_diameter"] = 0.2
        smart["lead_diameter"] = 1.65
    return rows


def render(rows: list[dict[str, object]]) -> str:
    return json.dumps(rows, indent=0) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if the catalogue on disk differs from the spec table",
    )
    args = ap.parse_args()
    try:
        rows = build_rows()
    except CatalogueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    text = render(rows)
    if args.check:
        have = OUT.read_text(encoding="utf-8") if OUT.is_file() else ""
        if have != text:
            print(f"{OUT} is stale; re-run scripts/emit_electrode_catalogue.py")
            return 1
        print(f"{OUT} up to date")
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8")
    print(OUT, len(rows))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
