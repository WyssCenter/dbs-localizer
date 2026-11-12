# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

from __future__ import annotations

import argparse
import copy
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

OUT_DIR = (
    Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "reports" / "synthetic"
)

PAGE_H = 792.0
NBSP = " "

_NARROW = {
    " ": 278,
    "!": 278,
    '"': 355,
    "#": 556,
    "$": 556,
    "%": 889,
    "&": 667,
    "'": 191,
    "(": 333,
    ")": 333,
    "*": 389,
    "+": 584,
    ",": 278,
    "-": 333,
    ".": 278,
    "/": 278,
    ":": 278,
    ";": 278,
    "<": 584,
    "=": 584,
    ">": 584,
    "?": 556,
    "@": 1015,
    "[": 278,
    "\\": 278,
    "]": 278,
    "^": 469,
    "_": 556,
    "`": 333,
    "{": 334,
    "|": 260,
    "}": 334,
    "~": 584,
    " ": 278,
    "°": 400,
    "µ": 556,
    "–": 556,
}
for _c, _w in zip(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    [667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833]
    + [722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611],
    strict=True,
):
    _NARROW[_c] = _w
for _c, _w in zip(
    "abcdefghijklmnopqrstuvwxyz",
    [556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833]
    + [556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500],
    strict=True,
):
    _NARROW[_c] = _w
for _c in "0123456789":
    _NARROW[_c] = 556


def char_width(c: str) -> int:
    return _NARROW.get(c, 556)


def text_width(s: str, size: float) -> float:
    return sum(char_width(c) for c in s) / 1000.0 * size


def num(v: float) -> str:
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("", "-0") else s


def pdf_string(s: str) -> str:
    out = []
    for b in s.encode("cp1252"):
        ch = chr(b)
        if 32 <= b < 127 and ch not in "()\\":
            out.append(ch)
        else:
            out.append(f"\\{b:03o}")
    return "(" + "".join(out) + ")"


class Page:
    def __init__(self) -> None:
        self.ops: list[str] = []

    def text(
        self,
        x: float,
        base: float,
        s: str,
        size: float = 12.0,
        bold: bool = False,
        align: str = "l",
        white: bool = False,
    ) -> None:
        width = text_width(s, size)
        if align == "r":
            x -= width
        elif align == "c":
            x -= width / 2.0
        font = "F2" if bold else "F1"
        op = (
            f"BT /{font} {num(size)} Tf 1 0 0 1 {num(x)} {num(PAGE_H - base)} Tm "
            f"{pdf_string(s)} Tj ET"
        )
        self.ops.append(f"q 1 g {op} Q" if white else op)

    def rotated(self, left: float, base: float, s: str, size: float = 6.7) -> None:
        self.ops.append(
            f"BT /F1 {num(size)} Tf 0 1 -1 0 {num(left + size)} {num(PAGE_H - base)} Tm "
            f"{pdf_string(s)} Tj ET"
        )

    def rect(
        self,
        x: float,
        top: float,
        w: float,
        h: float,
        lw: float = 0.3,
        fill: tuple[float, float, float] | None = None,
        stroke: bool = True,
        alpha: str | None = None,
    ) -> None:
        y = PAGE_H - top - h
        body = []
        if alpha:
            body.append(f"/{alpha} gs")
        if fill is not None:
            body.append(f"{num(fill[0])} {num(fill[1])} {num(fill[2])} rg")
        if stroke:
            body.append(f"{num(lw)} w")
        paint = "B" if stroke and fill is not None else ("S" if stroke else "f")
        body.append(f"{num(x)} {num(y)} {num(w)} {num(h)} re {paint}")
        self.ops.append("q " + " ".join(body) + " Q")

    def line(self, x0: float, y0: float, x1: float, y1: float, lw: float) -> None:
        self.ops.append(
            f"q {num(lw)} w {num(x0)} {num(PAGE_H - y0)} m {num(x1)} {num(PAGE_H - y1)} l S Q"
        )

    def circle(self, cx: float, cy: float, r: float, lw: float = 0.4) -> None:
        k = 0.5522847498 * r
        x, y = cx, PAGE_H - cy
        path = (
            f"{num(x + r)} {num(y)} m "
            f"{num(x + r)} {num(y + k)} {num(x + k)} {num(y + r)} {num(x)} {num(y + r)} c "
            f"{num(x - k)} {num(y + r)} {num(x - r)} {num(y + k)} {num(x - r)} {num(y)} c "
            f"{num(x - r)} {num(y - k)} {num(x - k)} {num(y - r)} {num(x)} {num(y - r)} c "
            f"{num(x + k)} {num(y - r)} {num(x + r)} {num(y - k)} {num(x + r)} {num(y)} c h"
        )
        self.ops.append(f"q 1 g {path} f Q")
        self.ops.append(f"q 0 G {num(lw)} w {path} S Q")

    def image(self, name: str, x: float, top: float, w: float, h: float) -> None:
        self.ops.append(
            f"q {num(w)} 0 0 {num(h)} {num(x)} {num(PAGE_H - top - h)} cm /{name} Do Q"
        )

    def stream(self) -> bytes:
        return ("\n".join(self.ops) + "\n").encode("ascii")


def hex_image(w: int, h: int, pixels: list[int]) -> bytes:
    data = "".join(f"{p:02x}" for p in pixels) + ">"
    return (
        f"<< /Type /XObject /Subtype /Image /Width {w} /Height {h} "
        f"/ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode "
        f"/Length {len(data)} >>\nstream\n{data}\nendstream"
    ).encode("ascii")


EYE_PIXELS = [
    255, 255, 128, 128, 128, 128, 255, 255,
    255, 128, 255, 255, 255, 255, 128, 255,
    128, 255, 255, 0, 0, 255, 255, 128,
    255, 128, 255, 255, 255, 255, 128, 255,
    255, 255, 128, 128, 128, 128, 255, 255,
]  # fmt: skip
OK_PIXELS = [
    255, 255, 255, 255, 255, 0,
    255, 255, 255, 0, 255, 255,
    0, 255, 255, 255, 255, 0,
    0, 255, 255, 255, 255, 255,
]  # fmt: skip

TO_UNICODE = b"""/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfrange
<20> <7E> <0020>
endbfrange
4 beginbfchar
<A0> <00A0>
<B0> <00B0>
<B5> <00B5>
<96> <2013>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end"""

ALPHAS = {"A90": 0.9, "A60": 0.6, "A30": 0.3}


def write_pdf(pages: list[Page]) -> bytes:
    objects: dict[int, bytes] = {}
    first_page = 7
    kids = " ".join(f"{first_page + 2 * i} 0 R" for i in range(len(pages)))
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R >>"
    objects[2] = f"<< /Type /Pages /Count {len(pages)} /Kids [{kids}] >>".encode(
        "ascii"
    )
    widths = " ".join(
        str(char_width(bytes([c]).decode("cp1252", errors="replace")))
        for c in range(32, 256)
    )
    cmap_id = first_page + 2 * len(pages)
    for obj, base in ((3, "Helvetica"), (4, "Helvetica-Bold")):
        objects[obj] = (
            f"<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding "
            f"/FirstChar 32 /LastChar 255 /Widths [{widths}] /ToUnicode {cmap_id} 0 R >>"
        ).encode("ascii")
    objects[cmap_id] = (
        f"<< /Length {len(TO_UNICODE)} >>\nstream\n".encode("ascii")
        + TO_UNICODE
        + b"\nendstream"
    )
    objects[5] = hex_image(8, 5, EYE_PIXELS)
    objects[6] = hex_image(6, 4, OK_PIXELS)
    gs = " ".join(f"/{k} << /ca {v} >>" for k, v in ALPHAS.items())
    resources = (
        "<< /Font << /F1 3 0 R /F2 4 0 R >> /XObject << /Eye 5 0 R /Ok 6 0 R >> "
        f"/ExtGState << {gs} >> >>"
    )
    for i, page in enumerate(pages):
        pid = first_page + 2 * i
        body = page.stream()
        objects[pid] = (
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources {resources} "
            f"/Contents {pid + 1} 0 R >>"
        ).encode("ascii")
        objects[pid + 1] = (
            f"<< /Length {len(body)} >>\nstream\n".encode("ascii") + body + b"endstream"
        )
    out = bytearray(b"%PDF-1.4\n")
    offsets: dict[int, int] = {}
    for oid in sorted(objects):
        offsets[oid] = len(out)
        out += f"{oid} 0 obj\n".encode("ascii") + objects[oid] + b"\nendobj\n"
    size = max(objects) + 1
    xref_at = len(out)
    out += f"xref\n0 {size}\n".encode("ascii") + b"0000000000 65535 f \n"
    for oid in range(1, size):
        if oid in offsets:
            out += f"{offsets[oid]:010d} 00000 n \n".encode("ascii")
        else:
            out += b"0000000000 65535 f \n"
    out += (
        f"trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"
    ).encode("ascii")
    return bytes(out)


@dataclass
class Prog:
    marks: dict[str, str]
    amp: float
    rate: int
    pw: int = 60
    unit: str = "mA"
    currents: dict[str, float] = field(default_factory=dict)
    eye: tuple[str, ...] = ()
    collapsed: tuple[int, ...] = ()
    bs_mode: str | None = None
    bs_status: str | None = None
    adaptive: dict[str, Any] | None = None

    def cathodes(self) -> list[str]:
        return [k for k, v in self.marks.items() if v == "-"]

    def anodes(self) -> list[str]:
        return [k for k, v in self.marks.items() if v == "+"]


def P(
    marks: str,
    amp: float,
    rate: int,
    cur: str = "",
    unit: str = "mA",
    eye: str = "",
    collapsed: tuple[int, ...] = (),
    bs: tuple[str, str] | None = None,
    adaptive: dict[str, Any] | None = None,
) -> Prog:
    parsed: dict[str, str] = {}
    for item in [m for m in marks.split(",") if m]:
        parsed[item[:-1]] = item[-1]
    currents = {}
    for item in [c for c in cur.split(",") if c]:
        k, v = item.split(":")
        currents[k] = float(v)
    return Prog(
        marks=parsed,
        amp=amp,
        rate=rate,
        unit=unit,
        currents=currents,
        eye=tuple(e for e in eye.split(",") if e),
        collapsed=collapsed,
        bs_mode=bs[0] if bs else None,
        bs_status=bs[1] if bs else None,
        adaptive=adaptive,
    )


def all_labels(first: int, directional: bool) -> list[str]:
    out: list[str] = []
    for level in range(4):
        if directional and level in (1, 2):
            out += [f"{first + level}{seg}" for seg in "abc"]
        else:
            out.append(str(first + level))
    return out


@dataclass
class Geometry:
    diameter: float
    pitch: float
    dx: float
    shaft_h: float


STIM = Geometry(10.0, 13.0, 13.0, 52.0)
NOTE = Geometry(6.0, 7.5, 8.0, 30.0)


def draw_diagram(
    pg: Page,
    shaft_x0: float,
    top: float,
    prog: Prog,
    first: int,
    directional: bool,
    geo: Geometry,
) -> None:
    pg.rect(shaft_x0, top, 2.0, geo.shaft_h, fill=(0, 0, 0), stroke=False)
    cx = shaft_x0 + 1.0
    r = geo.diameter / 2.0
    for row in range(1, 5):
        level = 4 - row
        cy = top + geo.pitch * row
        if directional and level in (1, 2) and level not in prog.collapsed:
            slots = [
                (cx - geo.dx, f"{first + level}a"),
                (cx, f"{first + level}b"),
                (cx + geo.dx, f"{first + level}c"),
            ]
        elif directional and level in (1, 2):
            slots = [(cx, "*")]
        else:
            slots = [(cx, str(first + level))]
        for x, label in slots:
            pg.circle(x, cy, r)
            state = (
                "eye"
                if label == "*" or label in prog.eye
                else prog.marks.get(label, ".")
            )
            x0, bottom, ctop = x - r, cy + r, cy - r
            if state == "eye":
                pg.image("Eye", x0 + 2, ctop + 2, 5.88, 4.83)
            elif state == "-":
                if geo is STIM:
                    pg.text(x0 + 4, bottom - 2, "-", 8.7)
                else:
                    pg.text(x0 + 2, bottom, "-", 8.7)
            elif state == "+":
                pg.text(x0 + 3, bottom - 3, "+", 6.7)
            elif geo is STIM:
                pg.text(x0 + 4, bottom - 3, ".", 6.7, white=True)
            else:
                pg.text(x0 + 2, bottom - 1.5, ".", 6.7, white=True)


def amp_text(p: Prog, places: int = 1) -> str:
    return f"{p.amp:.{places}f} {p.unit}"


def program_text(pg: Page, shaft_x0: float, top: float, p: Prog) -> None:
    bottom = top + 52
    pg.text(shaft_x0 - 12, bottom + 16, amp_text(p), 8.7)
    pg.text(shaft_x0 - 12, bottom + 29, f"{p.pw} µs", 8.7)
    pg.text(shaft_x0 - 12, bottom + 42, f"{p.rate} Hz", 8.7)
    if len(p.cathodes()) >= 2 and p.currents:
        pg.line(shaft_x0 - 26, bottom + 46, shaft_x0 + 39, bottom + 46, 1.0)
        for i, (label, value) in enumerate(p.currents.items()):
            pg.text(
                shaft_x0 - 26, bottom + 58 + 13 * i, f"{label}: {value:.1f} mA", 8.7
            )


def brainsense_text(pg: Page, shaft_x0: float, top: float, p: Prog) -> None:
    x = shaft_x0 + 32
    mode = "Adaptive" if p.bs_mode == "adaptive" else "Sensing Only"
    pg.text(x, top + 7, f"BrainSense: {mode}", 8.0)
    status = {"adapting": "Adapting", "paused": "Paused"}.get(p.bs_status or "")
    if status:
        pg.text(x, top + 17, f"Adaptive Therapy: {status}", 8.0)
    pg.text(x, top + 30, amp_text(p), 8.7)
    pg.text(x, top + 43, f"{p.pw} µs", 8.7)
    pg.text(x, top + 56, f"{p.rate} Hz", 8.7)
    if len(p.cathodes()) >= 2 and p.currents:
        pg.line(x, top + 60, x + 100, top + 60, 1.0)
        for i, (label, value) in enumerate(p.currents.items()):
            pg.text(x, top + 72 + 13 * i, f"{label}: {value:.1f} mA", 8.7)


def adaptive_block(pg: Page, x0: float, top: float, a: dict[str, Any]) -> None:
    x = x0 + 10
    pg.text(x, top + 150, "SENSING SETUP", 12.0)
    pg.text(x, top + 168, f"LFP Frequency: {a['lfp']:.2f} Hz", 8.7)
    pg.text(x, top + 186, "THRESHOLD MODE", 12.0)
    pg.text(x, top + 200, f"Mode: {a['mode']}", 8.7)
    pg.text(x, top + 216, "Upper LFP Threshold:", 8.7)
    pg.text(x, top + 232, f"{a['upper']}", 12.0)
    pg.text(x, top + 245, f"Captured: {a['upper_cap']}", 8.7)
    pg.text(x + 100, top + 216, "Lower LFP Threshold:", 8.7)
    pg.text(x + 100, top + 232, f"{a['lower']}", 12.0)
    pg.text(x + 100, top + 245, f"Captured: {a['lower_cap']}", 8.7)
    pg.text(x, top + 265, "ADAPTIVE SETUP", 12.0)
    pg.text(x, top + 283, f"Maximum Limit:{a['max']:.1f} mA", 8.7)
    pg.text(x, top + 299, f"Paused Amplitude: {a['paused']:.1f} mA", 10.0)
    pg.text(x, top + 315, f"Minimum Limit:{a['min']:.1f} mA", 8.7)
    pg.text(x, top + 331, "Transition Duration:", 8.7)
    pg.text(x, top + 345, f"Transition Up: {a['up']:.1f} mins", 8.7)
    pg.text(x, top + 359, f"Transition Down: {a['down']:.1f} mins", 8.7)


def stim_box(
    pg: Page,
    x0: float,
    top: float,
    active: bool,
    side: str,
    target: str,
    progs: list[Prog],
    first: int,
    directional: bool,
) -> None:
    brainsense = progs[0].adaptive is not None
    height = 400.0 if brainsense else 215.0
    lw = 2.0 if active else 0.3
    pg.rect(x0, top, 221.0, height, lw=lw)
    pg.text(x0 + 110.5, top - 10, f"{side.capitalize()} {target}", 12.0, align="c")
    if len(progs) == 2:
        pg.line(x0 + 110, top + 21, x0 + 110, top + 194, lw)
    for i, p in enumerate(progs):
        if brainsense:
            sx = x0 + 44
            draw_diagram(pg, sx, top + 6, p, first, directional, STIM)
            brainsense_text(pg, sx, top + 6, p)
            if p.adaptive is not None:
                adaptive_block(pg, x0, top, p.adaptive)
        else:
            sx = x0 + (54 if i == 0 else 164)
            draw_diagram(pg, sx, top + 6, p, first, directional, STIM)
            program_text(pg, sx, top + 6, p)


MONTHS = [
    "Jan",
    "Feb",
    "Mar",
    "Apr",
    "May",
    "Jun",
    "Jul",
    "Aug",
    "Sep",
    "Oct",
    "Nov",
    "Dec",
]


def fmt_date(d: tuple[int, int, int]) -> str:
    return f"{MONTHS[d[1] - 1]} {d[2]}, {d[0]}"


def fmt_datetime(d: tuple[int, int, int, int, int]) -> str:
    h = d[3] % 12 or 12
    ampm = "AM" if d[3] < 12 else "PM"
    return f"{MONTHS[d[1] - 1]} {d[2]}, {d[0]} {h}:{d[4]:02d} {ampm}"


def iso_date(d: tuple[int, int, int]) -> str:
    return f"{d[0]:04d}-{d[1]:02d}-{d[2]:02d}"


def iso_datetime(d: tuple[int, int, int, int, int]) -> str:
    return f"{d[0]:04d}-{d[1]:02d}-{d[2]:02d}T{d[3]:02d}:{d[4]:02d}"


@dataclass
class Report:
    slug: str
    kind: str
    directional: bool
    name: str
    pid: str
    app_version: str
    serial: str
    session: tuple[int, int, int, int, int]
    printed: tuple[int, int, int, int, int]
    diagnosis: str
    gender: str
    dob: tuple[int, int, int]
    notes: list[str]
    device: dict[str, str]
    battery_percent: int
    battery_estimate: str | None
    eri: tuple[int, int, int] | None
    implanted: tuple[int, int, int]
    reminder: str
    leads: dict[str, dict[str, Any]]
    initial: dict[str, Any]
    final: dict[str, Any]
    history: list[dict[str, Any]]
    columns: list[str]
    annotations: list[dict[str, Any]]
    survey: list[dict[str, Any]]
    identifier: dict[str, Any] | None
    events: dict[str, list[tuple[str, str | None]]]
    summary: dict[str, Any]
    usage: dict[str, Any]
    recharge: dict[str, Any] | None
    impedance: list[dict[str, Any]] = field(default_factory=list)

    @property
    def model(self) -> str:
        return f"Percept {self.kind}"

    @property
    def session_text(self) -> str:
        return fmt_datetime(self.session)

    @property
    def printed_text(self) -> str:
        return fmt_datetime(self.printed)


def chrome(pg: Page, rep: Report, number: int) -> None:
    pg.text(54, 35, "Session Report", 12.0)
    pg.text(54, 53, f"{rep.name} {rep.pid}", 12.0)
    pg.text(557.7, 53, f"Application Version: {rep.app_version}", 12.0, align="r")
    pg.text(54, 71, f"{rep.model} {rep.serial}", 12.0)
    pg.text(558.5, 71, f"Session Date: {rep.session_text}", 12.0, align="r")
    pg.line(54, 77, 558, 77, 3.0)
    pg.line(54, 772, 558, 772, 1.0)
    pg.text(54, 784, rep.printed_text, 12.0)
    pg.text(557.7, 784, f"Page {number}", 12.0, align="r")


def token(pg: Page, tag: str, base: float) -> None:
    pg.text(54, base, tag, 6.7, bold=True, white=True)


def title(pg: Page, base: float, s: str) -> None:
    pg.text(54, base, s, 18.0, bold=True)
    pg.line(54 + text_width(s, 18.0) + 6, base - 7, 558, base - 7, 0.0)


def flow(pg: Page, x: float, base: float, items: list[tuple[str, bool]]) -> None:
    for i, (s, bold) in enumerate(items):
        pg.text(x, base, s, 12.0, bold=bold)
        x += text_width(s, 12.0) + (7 if i % 2 == 0 else 20)


def page_identity(pg: Page, rep: Report) -> None:
    token(pg, "<s-pi>", 97)
    pg.text(54, 112, rep.name, 18.0, bold=True)
    flow(pg, 95, 150, [("Diagnosis:", True), (rep.diagnosis, False)])
    flow(
        pg,
        95,
        170,
        [
            ("Patient ID:", True),
            (rep.pid, False),
            ("Gender:", True),
            (rep.gender, False),
            ("Date Of Birth:", True),
            (fmt_date(rep.dob), False),
        ],
    )
    pg.text(95, 190, "Clinician Notes:", 12.0, bold=True)
    for i, line in enumerate(rep.notes):
        pg.text(95, 206 + 16 * i, line, 12.0)
    token(pg, "<e-pi>", 245)
    token(pg, "<s-di>", 273)
    pg.text(54, 263, "DEVICE", 18.0, bold=True)
    cols = (115, 270, 450) if rep.kind == "PC" else (120, 275, 455)
    d = rep.device

    def cell(col: int, base: float, label: str, value: str) -> None:
        pg.text(cols[col], base, label, 12.0, bold=True)
        pg.text(cols[col], base + 19, value, 12.0)

    cell(0, 303, "NEUROSTIMULATOR", rep.model)
    cell(1, 303, "Serial Number", rep.serial)
    cell(2, 303, "Implanted", fmt_date(rep.implanted))
    pg.text(cols[0], 347, "Product Application", 12.0, bold=True)
    pg.text(cols[0], 363, "ID:", 12.0, bold=True)
    pg.text(cols[0], 382, d["product_id"], 12.0)
    cell(1, 347, "Product Version:", d["product_version"])
    cell(2, 347, "Model Number", d["model_number"])
    cell(0, 410, "Core Application ID:", d["core_id"])
    cell(1, 410, "Core Version:", d["core_version"])
    cell(2, 410, "Device Name", d["device_name"])
    cell(0, 470, "Device Application ID:", d["device_id"])
    cell(1, 470, "Device Version:", d["device_version"])
    cell(2, 470, "Components", d["components"])
    cell(0, 530, "Battery Level", f"{rep.battery_percent}%")
    if rep.battery_estimate is not None:
        pg.text(cols[1], 530, "Estimated Battery Remaining", 12.0, bold=True)
        pg.text(cols[1], 549, "(based on last 7 days)", 12.0)
        pg.text(cols[1], 568, rep.battery_estimate, 12.0)
    if rep.eri is not None:
        cell(1, 530, "ERI Date", fmt_date(rep.eri))
    token(pg, "<e-di>", 651)
    pg.text(cols[0], 653, "Feature Information", 12.0, bold=True)
    pg.text(cols[0], 669, "Code", 12.0, bold=True)
    pg.text(cols[0], 688, d["feature_code"], 12.0)


def lead_drawing(
    pg: Page, x0: float, top: float, first: int, directional: bool
) -> None:
    cx = x0 + 53
    pg.rect(cx - 1, top + 15, 2.0, 73.0, fill=(0, 0, 0), stroke=False)
    for row in range(4):
        level = 3 - row
        cy = top + 33.45 + 18.45 * row
        if directional and level in (1, 2):
            slots = [
                (cx - 18, f"{first + level}a"),
                (cx, f"{first + level}b"),
                (cx + 18, f"{first + level}c"),
            ]
        else:
            slots = [(cx, str(first + level))]
        for x, label in slots:
            pg.circle(x, cy, 8.0)
            pg.text(x, cy + 2.3, label, 6.7, align="c")


def page_stim_header(pg: Page, rep: Report) -> None:
    token(pg, "<s-ss>", 97)
    title(pg, 117, "STIMULATION SETTINGS")
    pg.text(178, 148, "Initial", 12.0)
    pg.text(420, 148, "Final", 12.0)
    pg.text(80, 173, "STIMULATION", 12.0)
    pg.text(200, 173, rep.initial["stim"], 12.0)
    pg.text(424, 173, rep.final["stim"], 12.0)
    pg.text(80, 192, "CHECK BATTERY", 12.0)
    pg.text(80, 207, "REMINDER", 12.0)
    pg.text(200, 192, rep.reminder, 12.0)
    pg.text(424, 192, rep.reminder, 12.0)
    pg.text(80, 225, "CLOSE", 12.0)
    pg.text(80, 240, "COMMUNICATION", 12.0)
    pg.text(80, 255, "REQUIRED FOR", 12.0)
    pg.text(80, 270, "THERAPY OFF", 12.0)
    pg.text(200, 225, "ON", 12.0)
    pg.text(424, 225, "ON", 12.0)
    pg.text(80, 348, "Lead Configuration", 12.0, bold=True)
    for side, x0 in (("left", 74), ("right", 194), ("left", 329), ("right", 449)):
        lead = rep.leads[side]
        pg.rect(x0, 378, 106.0, 170.0, lw=0.3)
        pg.text(x0 + 25, 372, f"{side.capitalize()} {lead['target']}", 12.0)
        lead_drawing(pg, x0, 378, lead["first"], rep.directional)
        pg.text(x0 + 10, 484, f"Contacts: {lead['first']}–{lead['last']}", 8.7)
        pg.text(x0 + 10, 494, f"Lead Model: {lead['model']}", 8.7)
        if lead.get("orientation") is not None:
            pg.text(x0 + 10, 504, f"Orientation: {lead['orientation']} °", 8.7)


def group_heading(gid: str, name: str | None) -> str:
    return f"Group{NBSP}{gid}" + (f"{NBSP}({name})" if name else "")


def group_top(gid: str, side: str) -> float:
    if gid == "D":
        return 145.0 if side == "left" else 129.0
    return 145.0 if side == "left" else 391.0


def page_group(pg: Page, rep: Report, gid: str, sides: tuple[str, ...]) -> None:
    for snapshot, x0 in ((rep.initial, 60.0), (rep.final, 315.0)):
        group = snapshot["groups"][gid]
        for side in sides:
            top = group_top(gid, side)
            if side == sides[0] and not (gid == "D" and side == "right"):
                pg.text(
                    x0, top - 34, group_heading(gid, group["name"]), 12.0, bold=True
                )
            stim_box(
                pg,
                x0,
                top,
                snapshot["active"] == gid,
                side,
                rep.leads[side]["target"],
                group[side],
                rep.leads[side]["first"],
                rep.directional,
            )


def page_pps(pg: Page, rep: Report) -> None:
    token(pg, "<s-pps>", 97)
    title(pg, 117, "PATIENT PROGRAMMER SETTINGS")
    pg.text(151, 132, "Initial", 12.0)
    pg.text(405, 132, "Final", 12.0)
    for snapshot, xb in ((rep.initial, 74.0), (rep.final, 328.0)):
        for gi, gid in enumerate("ABCD"):
            y0 = 160.0 + 140.0 * gi
            group = snapshot["groups"][gid]
            pg.text(xb, y0, group_heading(gid, group["name"]), 12.0, bold=True)
            entry = snapshot["pps"][gid]
            for hi, side in enumerate(("left", "right")):
                yb = y0 + 18 + 60 * hi
                pg.text(
                    xb, yb, f"{side.capitalize()} {rep.leads[side]['target']}", 12.0
                )
                pg.text(xb, yb + 14, "Mode", 8.7, bold=True)
                pg.text(xb, yb + 26, entry["mode"], 8.7)
                xr = xb + 86
                lim = entry[side]
                if entry["mode"] == "Adaptive Therapy":
                    pg.text(xr, yb + 14, "Parameter:", 8.7)
                    pg.text(
                        xr, yb + 26, f"Paused Amplitude: {lim['paused']:.1f} mA", 8.7
                    )
                    pg.text(xb, yb + 38, "Adapting between", 8.7)
                    pg.text(xr, yb + 38, f"Maximum Limit: {lim['max']:.1f} mA", 8.7)
                    pg.text(xr, yb + 50, f"Minimum Limit: {lim['min']:.1f} mA", 8.7)
                else:
                    pg.text(
                        xr, yb + 14, f"Parameter: {entry['parameter']}", 8.7, bold=True
                    )
                    if lim is not None:
                        pg.text(
                            xr,
                            yb + 26,
                            f"Program 1: {lim['lower']:.1f} mA to {lim['upper']:.1f} mA",
                            8.7,
                        )
                        pg.text(xr, yb + 38, f"Value : {lim['value']:.1f} mA", 8.7)
    token(pg, "<e-pps>", 765)


def page_gs(pg: Page, rep: Report) -> None:
    token(pg, "<s-gs>", 97)
    title(pg, 117, "GROUP SETTINGS")
    for snapshot, xb in ((rep.initial, 60.0), (rep.final, 329.0)):
        for gi, gid in enumerate("ABCD"):
            y0 = 150.0 + 110.0 * gi
            group = snapshot["groups"][gid]
            pg.rect(xb, y0 - 16, 241.0, 90.0, lw=2.0)
            pg.text(xb + 8, y0, group_heading(gid, group["name"]), 12.0, bold=True)
            gset = snapshot["gs"][gid]
            pg.text(xb + 8, y0 + 18, f"SoftStart/Stop: {gset['soft']}", 8.7)
            pg.text(xb + 8, y0 + 30, f"Cycling: {gset['cycling']}", 8.7)


def page_gs_sensing(pg: Page, rep: Report) -> None:
    for snapshot, xb in ((rep.initial, 60.0), (rep.final, 329.0)):
        name = snapshot["groups"]["D"]["name"]
        pg.text(xb + 8, 130, group_heading("D", name), 12.0, bold=True)
        y = 154.0
        for side in ("left", "right"):
            s = snapshot["gs"]["D"]["sensing"][side]
            target = rep.leads[side]["target"]
            pg.text(xb + 8, y, f"{side.capitalize()} {target}", 12.0, bold=True)
            lines = [
                f"High Pass Filter: {s['hpf']} Hz",
                f"Averaging Duration: {s['avg']:.1f} secs",
                f"Sensing Blanking Duration: {s['blank']:.1f} ms",
                "Onset Duration:",
                f"Upper: {s['upper']:.1f} secs",
                f"Lower: {s['lower']:.1f} secs",
                f"Detection Blanking Duration: {s['detect']:.1f} secs",
                f"Adaptive Startup Delay: {s['delay']:.1f} secs",
            ]
            for i, line in enumerate(lines):
                pg.text(xb + 8, y + 16 + 12 * i, line, 8.7)
            y += 16 + 12 * len(lines) + 22
    token(pg, "<e-gs>", 765)


IMPEDANCE_CELL = 35.0


def ohm_text(entry: dict[str, Any]) -> str:
    return f"{entry.get('bound', '')}{entry['ohm']:,}"


def impedance_entries(labels: list[str], hemisphere: str) -> list[dict[str, Any]]:
    rows = list(reversed(labels))
    out: list[dict[str, Any]] = []
    seed = 7 if hemisphere == "left" else 19
    n = 0
    for r in rows:
        n += 1
        out.append(
            {
                "hemisphere": hemisphere,
                "kind": "monopolar",
                "a": "case",
                "b": r,
                "ohm": 600 + (seed * 53 + n * 211) % 1900,
            }
        )
    for r in rows:
        for c in labels[: labels.index(r)]:
            n += 1
            out.append(
                {
                    "hemisphere": hemisphere,
                    "kind": "bipolar",
                    "a": c,
                    "b": r,
                    "ohm": 1100 + (seed * 71 + n * 337) % 2600,
                }
            )
    return out


def draw_cell(pg: Page, x: float, top: float, entry: dict[str, Any]) -> None:
    pg.rect(x, top, IMPEDANCE_CELL, IMPEDANCE_CELL, lw=0.0)
    pg.image("Ok", x + 10, top + 10, 14.26, 9.43)
    pg.text(x + IMPEDANCE_CELL / 2, top + 33, ohm_text(entry), 8.7, align="c")


def draw_impedance_table(
    pg: Page,
    labels: list[str],
    mono_x: float,
    bip_x: float,
    top: float,
    cells: dict[tuple[str, str, str], dict[str, Any]],
) -> None:
    pg.text(mono_x - 16, top - 15, "MONOPOLAR", 8.7)
    pg.text(bip_x + 21, top - 15, "BIPOLAR", 8.7)
    rows = list(reversed(labels))
    for i, r in enumerate(rows):
        cell_top = top + IMPEDANCE_CELL * i
        pg.text(mono_x - 16, cell_top + 28, r, 8.7)
        draw_cell(pg, mono_x, cell_top, cells[("case", r, "monopolar")])
        index = labels.index(r)
        if index > 0:
            pg.text(bip_x - 15, cell_top + 28, r, 8.7)
        for j in range(index):
            draw_cell(
                pg,
                bip_x + IMPEDANCE_CELL * j,
                cell_top,
                cells[(labels[j], r, "bipolar")],
            )
    for j, c in enumerate(labels[:-1]):
        pg.text(
            bip_x + IMPEDANCE_CELL * j + IMPEDANCE_CELL / 2, top - 7, c, 8.7, align="c"
        )


def page_impedance(pg: Page, rep: Report, sides: list[str]) -> None:
    token(pg, "<s-sii>", 97)
    pg.text(54, 134, "ELECTRODE IMPEDANCE", 12.0, bold=True)
    pg.text(54, 158, "Impedance measured at Fixed test current", 12.0)
    for k, side in enumerate(sides):
        labels = all_labels(rep.leads[side]["first"], rep.directional)
        cells = {
            (e["a"], e["b"], e["kind"]): e
            for e in rep.impedance
            if e["hemisphere"] == side
        }
        if len(sides) == 2:
            heading_x = 110.0 + 234.0 * k
            offset = 249.0 * k
            mono_x, bip_x = 70.0 + offset, 175.0 + offset
        else:
            heading_x = 110.0
            mono_x, bip_x = 70.0, 180.0
        pg.text(
            heading_x, 182, f"{side.capitalize()} {rep.leads[side]['target']}", 12.0
        )
        draw_impedance_table(pg, labels, mono_x, bip_x, 215.0, cells)
    if len(sides) == 2:
        pg.line(283, 170, 283, 560, 1.5)


def page_history(
    pg: Page,
    rep: Report,
    placed: list[tuple[int, int]],
    headings: bool,
    tail: list[tuple[str, float, float]],
    is_first: bool,
) -> None:
    if is_first:
        token(pg, "<s-gh>", 97)
        title(pg, 117, "GROUP HISTORY")
    for index, col in placed:
        session = rep.history[index]
        x0 = 75.0 if col == 0 else 344.0
        hx = 60.0 if col == 0 else 329.0
        if headings:
            pg.text(hx, 132, fmt_date(session["date"]), 12.0)
        for gi, gid in enumerate("ABCD"):
            top = 146.0 + 110.0 * gi
            pg.rect(x0, top, 240.0, 110.0, lw=0.3)
            pg.text(hx, top + 58, gid, 12.0, bold=True)
            for side, base_slot in (("left", 0), ("right", 2)):
                first_contact = rep.leads[side]["first"]
                for pi, p in enumerate(session["groups"][gid][side]):
                    slot = base_slot + pi
                    bs = p.bs_mode is not None
                    sx = x0 + 60 * slot + (21 if bs else 29)
                    draw_diagram(
                        pg, sx, top + 6, p, first_contact, rep.directional, STIM
                    )
                    tx = sx - (6 if bs else 12)
                    pg.text(tx, top + 74, amp_text(p), 8.7)
                    pg.text(tx, top + 87, f"{p.pw} µs", 8.7)
                    pg.text(tx, top + 100, f"{p.rate} Hz", 8.7)
                    if bs:
                        mode = "Adaptive" if p.bs_mode == "adaptive" else "Sensing Only"
                        pg.text(sx + 24, top + 16, "BrainSense:", 7.0)
                        pg.text(sx + 24, top + 26, mode, 7.0)
                        if p.bs_status:
                            pg.text(sx + 24, top + 36, "Adaptive Therapy:", 7.0)
                            pg.text(sx + 24, top + 46, p.bs_status.capitalize(), 7.0)
    for label, x, base in tail:
        pg.text(x, base, label, 12.0)


def page_usage(pg: Page, rep: Report) -> None:
    token(pg, "<s-gu>", 97)
    title(pg, 117, "GROUP USAGE")
    for i, g in enumerate("ABCD"):
        pg.text(515, 131 + 19 * i, f"Group {g}", 12.0)
    for tick, y in zip((100, 80, 60, 40, 20), (180, 270, 360, 450, 540), strict=True):
        pg.text(94, y, str(tick), 10.0, align="r")
    for m in rep.usage["months"]:
        pg.rect(
            m["x"] - 25, 320, 50.0, 205.0, lw=0.3, fill=(0.8, 0.8, 0.8), stroke=False
        )
        pg.text(m["x"], 300, f"{m['total']}%", 15.0, align="c")
        pg.text(m["x"], 535, m["label"], 10.0, align="c")
    token(pg, "<e-gu>", 420)


COUPLING_ALPHA = {"Excellent": "A90", "Good": "A60", "Fair": "A30"}


def recharge_level(percent: int) -> float:
    return 470.0 - 3.4 * (percent - 20)


def page_recharge(pg: Page, rep: Report) -> None:
    rc = rep.recharge
    if rc is None:
        return
    token(pg, "<s-rc>", 97)
    title(pg, 117, "BATTERY RECHARGE")
    for tick, y in zip((100, 80, 60, 40, 20), (180, 248, 316, 384, 452), strict=True):
        pg.text(94, y, str(tick), 10.0, align="r")
    for i, s in enumerate(rc["sessions"]):
        x0 = 80.0 + 70.0 * i
        top, bottom = recharge_level(s["end"]), recharge_level(s["start"])
        pg.rect(
            x0,
            top,
            27.0,
            bottom - top,
            lw=0.0,
            fill=(0.4, 0.4, 0.4),
            stroke=False,
            alpha=COUPLING_ALPHA[s["coupling"]],
        )
        pg.text(x0 + 13.5, 495, s["label"], 10.0, align="c")
        pg.text(x0 + 13.5, bottom - 2, f"{s['start']}%", 6.0, align="c")
        pg.text(x0 + 13.5, top + 8, f"{s['end']}%", 6.0, align="c")
        pg.text(x0 + 13.5, (top + bottom) / 2, s["duration"], 6.0, align="c")
    for i, name in enumerate(("Excellent", "Good", "Fair")):
        top = 450.0 + 20 * i
        pg.rect(
            420,
            top,
            20.0,
            15.0,
            lw=0.0,
            fill=(0.4, 0.4, 0.4),
            stroke=False,
            alpha=COUPLING_ALPHA[name],
        )
        pg.text(448, top + 11, name, 10.0)
    for i, (value, caption) in enumerate(rc["metrics"]):
        b = 560.0 + 40 * i
        pg.text(420, b, value, 14.0)
        pg.text(420, b + 14, caption, 10.0)
    token(pg, "<e-rc>", 760)


def page_annotations(pg: Page, rep: Report) -> None:
    token(pg, "<s-sa>", 97)
    title(pg, 117, "SESSION ANNOTATIONS")
    for i, name in enumerate(rep.columns):
        pg.rotated(195.3 + 61.0 * i, 195, name)
    y = 213.0
    for side in ("left", "right"):
        rows = [a for a in rep.annotations if a["side"] == side]
        pg.text(60, y, f"{side.upper()} HEMISPHERE", 12.0, bold=True)
        pg.text(60, y + 9, "CONFIGURATION", 6.7)
        b = y + 18
        for a in rows:
            prog: Prog = a["prog"]
            pg.text(65, b, fmt_date(a["date"]), 6.7)
            pg.text(65, b + 11, f"{prog.amp:.2f} {prog.unit}", 6.7)
            pg.text(65, b + 24, f"{prog.pw} µs", 6.7)
            pg.text(65, b + 37, f"{prog.rate} Hz", 6.7)
            draw_diagram(
                pg, 99.0, b + 5, prog, rep.leads[side]["first"], rep.directional, NOTE
            )
            for i, score in enumerate(a["scores"]):
                cx = 195.3 + 61.0 * i + 3.35
                pg.text(
                    cx, b + 20, "-" if score is None else str(score), 6.7, align="c"
                )
                if score is not None:
                    pg.rect(cx - 12.5, b + 24, 25.0, 5.0, lw=0.3)
                    pg.rect(
                        cx - 12.5,
                        b + 24,
                        2.5 * score,
                        5.0,
                        lw=0.0,
                        fill=(0.3, 0.3, 0.3),
                        stroke=False,
                    )
            b += 52
        y = b + 8
    token(pg, "<e-sa>", 765)


def page_survey(pg: Page, rep: Report) -> None:
    token(pg, "<s-bss>", 97)
    title(pg, 117, "BRAINSENSE SURVEY")
    if not rep.survey:
        pg.text(70, 143, "No Data Available", 12.0, bold=True)
        token(pg, "<e-bss>", 765)
        return
    pg.text(71, 133, f"Right {rep.leads['right']['target']}", 14.0)
    for i, s in enumerate(rep.survey):
        x = 60.0 + 170.0 * (i % 3)
        row = i // 3
        pg.rect(x, 150.0 + 150.0 * row, 140.0, 80.0, lw=0.3)
        b = 237.0 + 150.0 * row
        pg.text(x, b, f"Sensing Electrodes: E{s['a']} - E{s['b']}", 7.5)
        pg.text(x, b + 10, f"Peak Magnitude: {s['magnitude']} µVp", 7.5)
        pg.text(x, b + 20, f"Peak Frequency: {s['frequency']:.2f} Hz", 7.5)
    token(pg, "<e-bss>", 765)


def page_identifier(pg: Page, rep: Report) -> None:
    token(pg, "<s-mbss>", 97)
    title(pg, 117, "ELECTRODE IDENTIFIER")
    ident = rep.identifier
    if ident is None:
        pg.text(70, 143, "No Data Available", 12.0, bold=True)
        token(pg, "<e-mbss>", 765)
        return
    pg.text(70, 133, f"Right {rep.leads['right']['target']}", 14.0)
    pg.text(70, 160, "Detected Frequency:", 8.7)
    pg.text(70, 176, f"{ident['detected']:.2f} Hz", 8.7)
    pg.text(70, 196, "Selected Frequency:", 8.7)
    pg.text(70, 212, f"{ident['selected']:.2f} Hz", 8.7)
    pg.text(70, 232, "LFP Magnitude:", 8.7)
    for i, (contact, rank) in enumerate(ident["ranks"]):
        pg.text(80, 248 + 14 * i, contact, 8.7)
        pg.text(100, 248 + 14 * i, rank, 8.7)
    token(pg, "<e-mbss>", 765)


def page_events(pg: Page, rep: Report) -> None:
    token(pg, "<s-pe>", 97)
    title(pg, 117, "PATIENT EVENTS")
    pg.text(164, 148, "Initial", 12.0)
    pg.text(420, 148, "Final", 12.0)
    for snap, lx, vx, bx in (
        ("initial", 90.0, 167.0, 74.0),
        ("final", 345.0, 422.0, 329.0),
    ):
        top = 160.0
        for name, extra in rep.events[snap]:
            height = 45.0 if extra else 30.0
            pg.rect(bx, top, 226.0, height, lw=2.0)
            pg.text(lx, top + 16, "Name:", 8.7)
            pg.text(vx, top + 16, name, 8.7)
            if extra:
                pg.text(lx, top + 30, f"Additional Behavior:{extra}", 8.7)
            top += height + 15
    token(pg, "<e-pe>", 765)


def page_summary(pg: Page, rep: Report) -> None:
    token(pg, "<s-es>", 97)
    title(pg, 117, "EVENT SUMMARY")
    s = rep.summary
    pg.text(54, 140, "Events", 12.0, bold=True)
    pg.text(251.8, 140, s["range"], 12.0, bold=True, align="r")
    b = 164.0
    for name, count in s["events"]:
        pg.text(54, b, name, 12.0)
        pg.text(251.8, b, str(count), 12.0, align="r")
        b += 24
    for col, side in ((54.0, "left"), (335.0, "right")):
        block = s["lfp"][side]
        by = 300.0 if side == "left" else 140.0
        right = 251.8 if side == "left" else 532.8
        target = rep.leads[side]["target"]
        pg.text(col, by, f"Group D {side.capitalize()} {target}", 12.0, bold=True)
        pg.text(right, by, s["range"], 12.0, bold=True, align="r")
        rows = [
            ("Above Threshold", f"{block['above']}%"),
            ("Between Thresholds", f"{block['between']}%"),
            ("Below Threshold", f"{block['below']}%"),
            ("Average Amplitude", f"{block['average']:.2f}"),
            ("Adaptive Amplitude", f"{block['reduction']}%"),
            ("Time Adaptive Running", f"{block['running']}%"),
        ]
        yy = by + 24
        for label, value in rows:
            pg.text(col, yy, label, 12.0)
            pg.text(right, yy, value, 12.0, align="r")
            if label == "Adaptive Amplitude":
                yy += 14
                pg.text(col, yy, "Reduction", 12.0)
            yy += 24
    pg.text(54, 760, "END OF REPORT", 12.0)
    token(pg, "<e-s-es>", 765)


def build_pages(rep: Report) -> list[Page]:
    pages: list[Page] = []

    def new() -> Page:
        pg = Page()
        pages.append(pg)
        return pg

    page_identity(new(), rep)
    page_stim_header(new(), rep)
    for gid in "ABC":
        page_group(new(), rep, gid, ("left", "right"))
    page_group(new(), rep, "D", ("left",))
    last_group = new()
    page_group(last_group, rep, "D", ("right",))
    token(last_group, "<e-ss>", 765)
    page_pps(new(), rep)
    page_gs(new(), rep)
    page_gs_sensing(new(), rep)
    for sides in [["left"], ["right"]] if rep.directional else [["left", "right"]]:
        page_impedance(new(), rep, sides)
    page_history(
        new(),
        rep,
        [(0, 0), (1, 1)],
        True,
        [
            (fmt_date(rep.history[2]["date"]), 60.0, 627.0),
            (fmt_date(rep.history[3]["date"]), 329.0, 627.0),
        ],
        True,
    )
    last_history = new()
    page_history(last_history, rep, [(2, 0), (3, 1)], False, [], False)
    token(last_history, "<e-gh>", 765)
    page_usage(new(), rep)
    if rep.recharge is not None:
        page_recharge(new(), rep)
    page_annotations(new(), rep)
    page_survey(new(), rep)
    page_identifier(new(), rep)
    page_events(new(), rep)
    page_summary(new(), rep)
    for i, pg in enumerate(pages):
        chrome(pg, rep, i + 1)
    return pages


ADAPTIVE_LEFT = {
    "lfp": 24.41,
    "mode": "Dual Threshold",
    "upper": 50,
    "upper_cap": 30,
    "lower": 20,
    "lower_cap": 18,
    "max": 3.2,
    "paused": 2.6,
    "min": 2.2,
    "up": 2.3,
    "down": 5.0,
}
ADAPTIVE_RIGHT = {
    "lfp": 19.53,
    "mode": "Single Threshold",
    "upper": 45,
    "upper_cap": 28,
    "lower": 15,
    "lower_cap": 12,
    "max": 3.0,
    "paused": 2.4,
    "min": 2.0,
    "up": 1.5,
    "down": 4.0,
}

GS_SENSING = {
    "left": {
        "hpf": 1,
        "avg": 1.2,
        "blank": 1.4,
        "upper": 1.2,
        "lower": 1.2,
        "detect": 2.0,
        "delay": 10.0,
    },
    "right": {
        "hpf": 1,
        "avg": 3.0,
        "blank": 1.1,
        "upper": 0.8,
        "lower": 1.6,
        "detect": 2.5,
        "delay": 8.0,
    },
}


def group_gs(soft: dict[str, str]) -> dict[str, Any]:
    return {
        gid: {
            "soft": soft[gid],
            "cycling": "Off",
            "sensing": GS_SENSING if gid == "D" else None,
        }
        for gid in "ABCD"
    }


def limits(p: Prog) -> dict[str, float]:
    return {
        "lower": round(p.amp - 0.4, 1),
        "upper": round(p.amp + 0.4, 1),
        "value": p.amp,
    }


def pps_for(groups: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for gid in "ABCD":
        g = groups[gid]
        if gid == "D":
            out[gid] = {
                "mode": "Adaptive Therapy",
                "left": {
                    "paused": ADAPTIVE_LEFT["paused"],
                    "max": ADAPTIVE_LEFT["max"],
                    "min": ADAPTIVE_LEFT["min"],
                },
                "right": {
                    "paused": ADAPTIVE_RIGHT["paused"],
                    "max": ADAPTIVE_RIGHT["max"],
                    "min": ADAPTIVE_RIGHT["min"],
                },
            }
        elif gid == "C":
            out[gid] = {
                "mode": "Advanced View",
                "parameter": "View Amplitude",
                "left": None,
                "right": None,
            }
        else:
            out[gid] = {
                "mode": "Advanced Adjust",
                "parameter": "Adjust Amplitude",
                "left": limits(g["left"][0]),
                "right": limits(g["right"][0]) if gid == "A" else None,
            }
    return out


def snapshot(
    stim: str, active: str, groups: dict[str, Any], soft: dict[str, str]
) -> dict[str, Any]:
    return {
        "stim": stim,
        "active": active,
        "groups": groups,
        "gs": group_gs(soft),
        "pps": pps_for(groups),
    }


def history_from(
    final: dict[str, Any], dates: list[tuple[int, int, int]]
) -> list[dict[str, Any]]:
    out = []
    for i, date in enumerate(dates):
        groups: dict[str, Any] = {}
        for gid in "ABCD":
            entry: dict[str, Any] = {}
            for side in ("left", "right"):
                progs = []
                for p in final["groups"][gid][side]:
                    q = copy.deepcopy(p)
                    q.amp = round(p.amp - 0.1 * i, 1)
                    q.currents = {}
                    q.adaptive = None
                    progs.append(q)
                entry[side] = progs
            groups[gid] = entry
        out.append({"date": date, "groups": groups})
    return out


DEVICE_FIELDS = {
    "product_id": "0000",
    "product_version": "01.02.03",
    "model_number": "TEST-PC-01",
    "core_id": "0000",
    "core_version": "00.01.00",
    "device_name": "--",
    "device_id": "0000",
    "device_version": "00.02.00",
    "components": "Left Chest",
    "feature_code": "0a1b",
}
HISTORY_DATES = [(2026, 10, 14), (2026, 7, 2), (2026, 6, 2), (2026, 5, 3)]
COLUMNS = [
    "TREMOR",
    "BRADYKINESIA",
    "MOOD ALTERATION",
    "MUSCLE CONTRACTIONS",
    "PARESTHESIA – TRANSIENT",
]
SUMMARY = {
    "range": "Jul 02 – Oct 14",
    "events": [("Medication taken", 12), ("Tremor episode", 4), ("Fall", 1)],
    "lfp": {
        "left": {
            "above": 41,
            "between": 30,
            "below": 29,
            "average": 2.62,
            "reduction": 12,
            "running": 87,
        },
        "right": {
            "above": 35,
            "between": 40,
            "below": 25,
            "average": 2.41,
            "reduction": 9,
            "running": 91,
        },
    },
}


def ring_report() -> Report:
    leads: dict[str, dict[str, Any]] = {
        "left": {"first": 0, "last": 3, "model": "3389", "target": "STN"},
        "right": {"first": 8, "last": 11, "model": "3389", "target": "STN"},
    }
    final_groups: dict[str, dict[str, Any]] = {
        "A": {
            "name": "Gait",
            "left": [P("0-,1-,2-,3+", 4.5, 130, cur="0:1.1,1:1.7,2:0.9")],
            "right": [P("10-", 2.8, 130)],
        },
        "B": {
            "name": "Speech",
            "left": [P("1-", 2.1, 125), P("2-", 0.9, 125)],
            "right": [P("9-", 2.4, 125)],
        },
        "C": {
            "name": "Scan",
            "left": [P("1-,2+", 1.3, 150)],
            "right": [P("9+,10-", 1.3, 100)],
        },
        "D": {
            "name": "Adaptive",
            "left": [
                P(
                    "1-",
                    2.6,
                    125,
                    eye="0,2",
                    bs=("adaptive", "adapting"),
                    adaptive=ADAPTIVE_LEFT,
                )
            ],
            "right": [
                P(
                    "9-,10-",
                    2.4,
                    125,
                    cur="9:1.2,10:1.2",
                    eye="8,11",
                    bs=("adaptive", "paused"),
                    adaptive=ADAPTIVE_RIGHT,
                )
            ],
        },
    }
    initial_groups = copy.deepcopy(final_groups)
    initial_groups["A"]["left"] = [P("0-,1-,2-,3+", 4.1, 130, cur="0:1.0,1:1.6,2:0.8")]
    initial_groups["B"]["left"] = [P("1-", 2.0, 125, unit="V"), P("2-", 0.8, 125)]
    initial_groups["C"]["name"] = None
    initial_groups["D"]["left"][0].amp = 2.4
    initial_groups["D"]["right"][0].amp = 2.2
    initial_groups["D"]["right"][0].currents = {"9": 1.1, "10": 1.1}
    initial_groups["D"]["right"][0].bs_status = "adapting"
    final = snapshot(
        "On",
        "D",
        final_groups,
        {"A": "4 Seconds", "B": "2 Seconds", "C": "Off", "D": "4 Seconds"},
    )
    initial = snapshot(
        "On",
        "B",
        initial_groups,
        {"A": "4 Seconds", "B": "Off", "C": "Off", "D": "3 Seconds"},
    )
    annotations = [
        {
            "side": "left",
            "date": (2026, 3, 30),
            "prog": P("1-", 2.50, 130),
            "scores": [2, 1, None, 0, 3],
        },
        {
            "side": "left",
            "date": (2026, 4, 14),
            "prog": P("0-,1-", 3.10, 130),
            "scores": [1, 1, 2, None, None],
        },
        {
            "side": "left",
            "date": (2026, 5, 6),
            "prog": P("2-", 1.80, 125),
            "scores": [None, 0, 1, 2, 1],
        },
        {
            "side": "right",
            "date": (2026, 4, 2),
            "prog": P("10-", 2.20, 130),
            "scores": [3, None, None, 1, 0],
        },
        {
            "side": "right",
            "date": (2026, 6, 9),
            "prog": P("9-,10-", 2.75, 130),
            "scores": [0, 2, 1, 1, None],
        },
    ]
    rep = Report(
        slug="ring_percept_pc",
        kind="PC",
        directional=False,
        name="Sample Patient",
        pid="SYN:0001",
        app_version="0.0.1",
        serial="SYN_PC_0001",
        session=(2026, 10, 14, 14, 25),
        printed=(2026, 10, 14, 15, 2),
        diagnosis="Parkinson's Disease",
        gender="Female",
        dob=(1961, 3, 17),
        notes=["Invented test record.", "Not a real patient."],
        device=dict(DEVICE_FIELDS),
        battery_percent=82,
        battery_estimate="3 years, 6 months",
        eri=None,
        implanted=(2026, 1, 20),
        reminder="09:30",
        leads=leads,
        initial=initial,
        final=final,
        history=history_from(final, HISTORY_DATES),
        columns=COLUMNS,
        annotations=annotations,
        survey=[
            {"a": "11", "b": "9", "magnitude": "0.512345678", "frequency": 22.46},
            {"a": "10", "b": "8", "magnitude": "1.250000000", "frequency": 19.53},
            {"a": "11", "b": "8", "magnitude": "0.073210987", "frequency": 24.41},
        ],
        identifier={
            "detected": 22.46,
            "selected": 22.46,
            "ranks": [
                ("11", "Most"),
                ("9", "Medium"),
                ("10", "Medium"),
                ("8", "Least"),
            ],
        },
        events={
            "initial": [("Medication taken", None), ("Tremor episode", "Left arm")],
            "final": [("Fall", None), ("Medication taken", "Late dose")],
        },
        summary=SUMMARY,
        usage={
            "months": [
                {"x": 150.0, "label": "Aug 2026", "total": 97},
                {"x": 250.0, "label": "Sep 2026", "total": 100},
                {"x": 350.0, "label": "Oct 2026", "total": 94},
            ]
        },
        recharge=None,
    )
    for side in ("left", "right"):
        rep.impedance += impedance_entries(
            all_labels(leads[side]["first"], False), side
        )
    return rep


def directional_report() -> Report:
    leads: dict[str, dict[str, Any]] = {
        "left": {
            "first": 0,
            "last": 7,
            "model": "B33005",
            "target": "STN",
            "orientation": 60,
        },
        "right": {
            "first": 8,
            "last": 15,
            "model": "B33005",
            "target": "STN",
            "orientation": 300,
        },
    }
    final_groups: dict[str, dict[str, Any]] = {
        "A": {
            "name": "Reach",
            "left": [P("1a-,1b-,1c-", 3.2, 130, cur="1a:1.0,1b:1.0,1c:1.0")],
            "right": [P("8-,9a-,9b-,9c-", 3.4, 130, cur="8:1.2,9a:1.0,9b:1.0,9c:1.0")],
        },
        "B": {
            "name": "Stand",
            "left": [P("1a-,1c-", 2.2, 125, cur="1a:1.5,1c:0.7"), P("2b-", 0.9, 125)],
            "right": [P("10a-,10b-,10c-", 2.4, 125, cur="10a:0.8,10b:0.8,10c:0.8")],
        },
        "C": {
            "name": "Imaging",
            "left": [
                P("1a-,1b-,1c-,2a+,2b+,2c+", 1.3, 150, cur="1a:0.4,1b:0.4,1c:0.4")
            ],
            "right": [
                P("9a+,9b+,9c+,10a-,10b-,10c-", 1.3, 100, cur="10a:0.4,10b:0.4,10c:0.4")
            ],
        },
        "D": {
            "name": "Sense",
            "left": [
                P(
                    "1a-,1b-",
                    2.6,
                    125,
                    cur="1a:1.0,1b:0.6",
                    eye="0",
                    collapsed=(2,),
                    bs=("adaptive", "adapting"),
                    adaptive=ADAPTIVE_LEFT,
                )
            ],
            "right": [
                P(
                    "10a-,10c-",
                    2.2,
                    125,
                    cur="10a:0.9,10c:0.7",
                    eye="8,11",
                    bs=("adaptive", "adapting"),
                    adaptive=ADAPTIVE_RIGHT,
                )
            ],
        },
    }
    initial_groups = copy.deepcopy(final_groups)
    initial_groups["A"]["left"] = [
        P("1a-,1b-,1c-", 2.9, 130, cur="1a:1.0,1b:1.0,1c:1.0")
    ]
    initial_groups["B"]["right"] = [P("10b-", 2.0, 125)]
    initial_groups["D"]["left"][0].amp = 2.3
    final = snapshot(
        "On",
        "B",
        final_groups,
        {"A": "4 Seconds", "B": "4 Seconds", "C": "Off", "D": "2 Seconds"},
    )
    initial = snapshot(
        "Off",
        "A",
        initial_groups,
        {"A": "4 Seconds", "B": "Off", "C": "Off", "D": "2 Seconds"},
    )
    annotations = [
        {
            "side": "left",
            "date": (2026, 5, 11),
            "prog": P("1a-,1b-,1c-", 2.40, 130),
            "scores": [2, 1, 0, None],
        },
        {
            "side": "left",
            "date": (2026, 6, 15),
            "prog": P("0-", 1.95, 130),
            "scores": [1, None, 2, 3],
        },
        {
            "side": "right",
            "date": (2026, 7, 21),
            "prog": P("10a-,10c-", 2.65, 125),
            "scores": [None, 0, 1, 2],
        },
    ]
    device = dict(DEVICE_FIELDS)
    device.update(
        model_number="TEST-RC-02",
        device_name="Unit B",
        components="Right Chest",
        feature_code="2c3d",
    )
    rep = Report(
        slug="directional_percept_rc",
        kind="RC",
        directional=True,
        name="Example Person",
        pid="SYN:0002",
        app_version="0.0.2",
        serial="SYN_RC_0002",
        session=(2026, 10, 14, 9, 40),
        printed=(2026, 10, 14, 10, 15),
        diagnosis="Essential Tremor",
        gender="Male",
        dob=(1955, 11, 2),
        notes=["Second invented record."],
        device=device,
        battery_percent=64,
        battery_estimate=None,
        eri=(2041, 2, 28),
        implanted=(2026, 2, 3),
        reminder="Off",
        leads=leads,
        initial=initial,
        final=final,
        history=history_from(final, HISTORY_DATES),
        columns=COLUMNS[:4],
        annotations=annotations,
        survey=[],
        identifier=None,
        events={
            "initial": [("Medication taken", "Early dose")],
            "final": [("Fall", None)],
        },
        summary=SUMMARY,
        usage={
            "months": [
                {"x": 150.0, "label": "Jul 2026", "total": 88},
                {"x": 250.0, "label": "Aug 2026", "total": 100},
                {"x": 350.0, "label": "Sep 2026", "total": 95},
                {"x": 450.0, "label": "Oct 2026", "total": 100},
            ]
        },
        recharge={
            "sessions": [
                {
                    "label": "10/02",
                    "start": 35,
                    "end": 72,
                    "duration": "42 min",
                    "minutes": 42.0,
                    "coupling": "Excellent",
                },
                {
                    "label": "10/06",
                    "start": 28,
                    "end": 64,
                    "duration": "1.1 hr",
                    "minutes": 66.0,
                    "coupling": "Good",
                },
                {
                    "label": "10/09",
                    "start": 41,
                    "end": 80,
                    "duration": "38 min",
                    "minutes": 38.0,
                    "coupling": "Fair",
                },
                {
                    "label": "10/12",
                    "start": 30,
                    "end": 58,
                    "duration": "1.0 hr",
                    "minutes": 60.0,
                    "coupling": "Excellent",
                },
            ],
            "metrics": [
                ("Excellent", "Recharge Coupling"),
                ("68%", "Avg. Ending"),
                ("45 min", "Avg. Recharge Time"),
                ("3.4 days", "Avg. Recharge Interval"),
            ],
        },
    )
    for side in ("left", "right"):
        rep.impedance += impedance_entries(all_labels(leads[side]["first"], True), side)
    rep.impedance[3]["ohm"] = 10000
    rep.impedance[3]["bound"] = ">"
    return rep


def contact_order(rep: Report, side: str) -> list[str]:
    return all_labels(rep.leads[side]["first"], rep.directional)


def prog_expected(
    rep: Report, side: str, p: Prog, with_currents: bool
) -> dict[str, Any]:
    order = contact_order(rep, side)
    cathodes = [c for c in order if p.marks.get(c) == "-"]
    anodes = [c for c in order if p.marks.get(c) == "+"]
    sensing = set(p.eye)
    for level in p.collapsed:
        sensing |= {f"{rep.leads[side]['first'] + level}{seg}" for seg in "abc"}
    currents: dict[str, float] = {}
    if with_currents:
        if p.currents:
            currents = {c: p.currents[c] for c in order if c in p.currents}
        elif len(cathodes) == 1:
            currents = {cathodes[0]: p.amp}
    out: dict[str, Any] = {
        "amplitude": p.amp,
        "unit": p.unit,
        "pulse_width_us": p.pw,
        "frequency_hz": p.rate,
        "cathodes": cathodes,
        "anodes": anodes,
        "currents": currents,
        "sensing": [c for c in order if c in sensing],
        "case": "off" if anodes else "anode",
    }
    if p.bs_mode:
        out["bs_mode"] = p.bs_mode
        out["bs_status"] = p.bs_status
    return out


def without_case(prog: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in prog.items() if k != "case"}


def diagnostics_expected(rep: Report) -> dict[str, int]:
    sums = 0
    interleaved = 0
    voltage = 0
    for snap in (rep.initial, rep.final):
        for gid in "ABCD":
            for side in ("left", "right"):
                progs: list[Prog] = snap["groups"][gid][side]
                interleaved += len(progs) == 2
                for p in progs:
                    voltage += p.unit == "V"
                    printed = [p.currents[c] for c in p.cathodes() if c in p.currents]
                    if len(p.currents) >= 2 and abs(sum(printed) - p.amp) > 0.05 + 1e-9:
                        sums += 1
    return {
        "contact_sum_warnings": sums,
        "interleaved_hemispheres": interleaved,
        "voltage_programs": voltage,
    }


def groups_expected(
    rep: Report, groups: dict[str, Any], with_currents: bool, names: bool
) -> list[dict[str, Any]]:
    out = []
    for gid in "ABCD":
        g = groups[gid]
        entry: dict[str, Any] = {"id": gid}
        if names:
            entry["name"] = g.get("name")
        entry["hemispheres"] = {
            side: [prog_expected(rep, side, p, with_currents) for p in g[side]]
            for side in ("left", "right")
        }
        out.append(entry)
    return out


def adaptive_expected(a: dict[str, Any]) -> dict[str, Any]:
    return {
        "lfp_hz": a["lfp"],
        "limits": {"lower": a["min"], "upper": a["max"]},
        "threshold_mode": a["mode"],
        "paused_amplitude_ma": a["paused"],
        "transition_up_min": a["up"],
        "transition_down_min": a["down"],
        "upper_lfp_threshold": float(a["upper"]),
        "upper_lfp_captured": float(a["upper_cap"]),
        "lower_lfp_threshold": float(a["lower"]),
        "lower_lfp_captured": float(a["lower_cap"]),
    }


def soft_expected(text: str) -> dict[str, Any]:
    if text == "Off":
        return {"enabled": False}
    return {"enabled": True, "duration_s": float(text.split()[0])}


def snapshot_expected(rep: Report, snap: dict[str, Any]) -> dict[str, Any]:
    groups = groups_expected(rep, snap["groups"], True, True)
    for entry in groups:
        gid = entry["id"]
        entry["active"] = snap["active"] == gid
        entry["soft_start_stop"] = soft_expected(snap["gs"][gid]["soft"])
        entry["cycling"] = {"enabled": False}
        pps = snap["pps"][gid]
        entry["programmer"] = {"mode": pps["mode"]}
        for side in ("left", "right"):
            lim = pps[side]
            if lim is None:
                continue
            if pps["mode"] == "Adaptive Therapy":
                entry["programmer"][side] = {
                    "paused_amplitude_ma": lim["paused"],
                    "maximum_limit_ma": lim["max"],
                    "minimum_limit_ma": lim["min"],
                }
            else:
                entry["programmer"][side] = {
                    "lower": lim["lower"],
                    "upper": lim["upper"],
                    "value_ma": lim["value"],
                }
    for entry, gid in zip(groups, "ABCD", strict=True):
        if gid == "D":
            for side in ("left", "right"):
                a = ADAPTIVE_LEFT if side == "left" else ADAPTIVE_RIGHT
                entry["hemispheres"][side][0]["adaptive"] = adaptive_expected(a)
            entry["sensing_settings"] = {
                side: {
                    "high_pass_filter_hz": float(s["hpf"]),
                    "averaging_duration_s": s["avg"],
                    "sensing_blanking_ms": s["blank"],
                    "onset_upper_s": s["upper"],
                    "onset_lower_s": s["lower"],
                    "detection_blanking_s": s["detect"],
                    "adaptive_startup_delay_s": s["delay"],
                }
                for side, s in GS_SENSING.items()
            }
    return {
        "stimulation": snap["stim"].lower(),
        "active": snap["active"],
        "groups": groups,
    }


def expected(rep: Report) -> dict[str, Any]:
    d = rep.device
    device: dict[str, Any] = {
        "model": rep.model,
        "rechargeable": rep.kind == "RC",
        "serial": rep.serial,
        "model_number": d["model_number"],
        "implanted": iso_date(rep.implanted),
        "location": d["components"].lower(),
        "battery_percent": float(rep.battery_percent),
        "feature_code": d["feature_code"],
        "firmware": {
            "product_application_id": d["product_id"],
            "product_version": d["product_version"],
            "core_application_id": d["core_id"],
            "core_version": d["core_version"],
            "device_application_id": d["device_id"],
            "device_version": d["device_version"],
        },
        "device_name": None if d["device_name"] == "--" else d["device_name"],
        "battery_estimate": rep.battery_estimate,
        "eri_date": iso_date(rep.eri) if rep.eri else None,
    }
    leads = []
    for side in ("left", "right"):
        lead = rep.leads[side]
        leads.append(
            {
                "hemisphere": side,
                "vendor_model": lead["model"],
                "target": lead["target"],
                "tip_offset": float(lead["first"]),
                "last_contact": lead["last"],
                "orientation_deg": float(lead["orientation"])
                if lead.get("orientation") is not None
                else None,
                "contacts": contact_order(rep, side),
            }
        )
    history = []
    for entry in rep.history:
        history.append(
            {
                "date": iso_date(entry["date"]),
                "groups": groups_expected(rep, entry["groups"], False, False),
            }
        )
    annotations = []
    for a in rep.annotations:
        side = a["side"]
        p: Prog = a["prog"]
        annotations.append(
            {
                "date": iso_date(a["date"]),
                "hemisphere": side,
                "program": without_case(prog_expected(rep, side, p, False)),
                "symptoms": {
                    name.lower(): score
                    for name, score in zip(rep.columns, a["scores"], strict=True)
                },
            }
        )
    impedances = [
        {
            "hemisphere": e["hemisphere"],
            "kind": e["kind"],
            "a": e["a"],
            "b": e["b"],
            "ohm": float(e["ohm"]),
            "bound": e.get("bound"),
        }
        for e in rep.impedance
    ]
    out: dict[str, Any] = {
        "file": f"{rep.slug}.pdf",
        "patient": {
            "name": rep.name,
            "patient_id": rep.pid,
            "diagnosis": rep.diagnosis,
            "sex": rep.gender.lower(),
            "date_of_birth": iso_date(rep.dob),
            "clinician_notes": "\n".join(rep.notes),
        },
        "session": {
            "date": iso_datetime(rep.session),
            "printed_at": iso_datetime(rep.printed),
            "programmer_version": rep.app_version,
            "stimulation_initial": rep.initial["stim"].lower(),
            "stimulation_final": rep.final["stim"].lower(),
            "battery_reminder": rep.reminder.lower()
            if ":" not in rep.reminder
            else rep.reminder,
            "close_communication_required": True,
        },
        "diagnostics": diagnostics_expected(rep),
        "device": device,
        "leads": leads,
        "snapshots": {
            "initial": snapshot_expected(rep, rep.initial),
            "final": snapshot_expected(rep, rep.final),
        },
        "history": history,
        "impedances": impedances,
        "annotations": annotations,
        "group_usage": {
            "groups": list("ABCD"),
            "months": [
                {
                    "month_start": f"{m['label'][4:]}-{MONTHS.index(m['label'][:3]) + 1:02d}-01",
                    "total_percent": float(m["total"]),
                }
                for m in rep.usage["months"]
            ],
        },
        "survey": [
            {
                "electrodes": [s["a"], s["b"]],
                "peak_magnitude_uvp": float(s["magnitude"]),
                "peak_frequency_hz": s["frequency"],
            }
            for s in rep.survey
        ],
        "identifier": None
        if rep.identifier is None
        else {
            "detected_frequency_hz": rep.identifier["detected"],
            "selected_frequency_hz": rep.identifier["selected"],
            "ranks": [
                {"contact": c, "rank": r.lower()} for c, r in rep.identifier["ranks"]
            ],
        },
        "patient_events": {
            snap: [{"name": n, "additional_behavior": x} for n, x in rep.events[snap]]
            for snap in ("initial", "final")
        },
        "event_summary": {
            "events": [{"name": n, "count": c} for n, c in rep.summary["events"]],
            "lfp": rep.summary["lfp"],
        },
        "recharge": None
        if rep.recharge is None
        else {
            "sessions": [
                {
                    "date": f"2026-{s['label'][:2]}-{s['label'][3:]}",
                    "start_percent": float(s["start"]),
                    "end_percent": float(s["end"]),
                    "duration_min": s["minutes"],
                    "coupling": s["coupling"],
                }
                for s in rep.recharge["sessions"]
            ],
            "metrics": {
                "coupling": "Excellent",
                "average_ending_percent": 68.0,
                "average_recharge_time_min": 45.0,
                "average_recharge_interval_days": 3.4,
            },
        },
    }
    return out


def reports() -> list[Report]:
    return [ring_report(), directional_report()]


def render(rep: Report) -> tuple[bytes, str]:
    pdf = write_pdf(build_pages(rep))
    meta = expected(rep)
    meta["pages"] = len(build_pages(rep))
    return pdf, json.dumps(meta, indent=2, ensure_ascii=False) + "\n"


def write_all(out_dir: Path) -> list[Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    written = []
    for rep in reports():
        pdf, meta = render(rep)
        pdf_path = out_dir / f"{rep.slug}.pdf"
        meta_path = out_dir / f"{rep.slug}.expected.json"
        pdf_path.write_bytes(pdf)
        meta_path.write_text(meta, encoding="utf-8", newline="\n")
        written += [pdf_path, meta_path]
    return written


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=OUT_DIR)
    args = parser.parse_args()
    for path in write_all(args.out_dir):
        print(path)


if __name__ == "__main__":
    main()
