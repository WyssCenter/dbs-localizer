# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Print the CHANGELOG.md section of one release (Keep a Changelog 1.1.0).

Usage: python scripts/release_notes.py <version-or-tag> [--changelog PATH] [--out FILE]

Finds the heading `## [X.Y.Z] - YYYY-MM-DD` and returns the lines below it, up to the next
`## ` heading or the first link reference definition. Exit 1 when the section is missing,
has no valid date, has no bullet item, or the version has more than one heading, so a
release cannot be cut without notes.
"""

from __future__ import annotations

import argparse
import datetime
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.\-]+)?$")
HEADING_RE = re.compile(r"^##\s+\[([^\]]+)\](.*)$")
DATE_RE = re.compile(r"^\s+-\s+(\d{4}-\d{2}-\d{2})\s*$")
LINK_REF_RE = re.compile(r"^\[[^\]]+\]: ")
BULLET_RE = re.compile(r"^\s*[-*] \S")


def extract_notes(text: str, version: str) -> str:
    version = version.removeprefix("v")
    if not VERSION_RE.match(version):
        raise ValueError(f"{version!r} is not a version of the form X.Y.Z")
    lines = text.splitlines()
    starts = [
        i
        for i, line in enumerate(lines)
        if (m := HEADING_RE.match(line)) and m.group(1) == version
    ]
    if not starts:
        raise ValueError(f"CHANGELOG.md has no section '## [{version}] - YYYY-MM-DD'")
    if len(starts) > 1:
        raise ValueError(
            f"CHANGELOG.md has {len(starts)} sections for version {version}"
        )
    start = starts[0]
    heading = HEADING_RE.match(lines[start])
    assert heading is not None
    rest = heading.group(2)
    date_match = DATE_RE.match(rest)
    if not date_match:
        raise ValueError(
            f"the heading of {version} has no date (expected '- YYYY-MM-DD')"
        )
    try:
        datetime.date.fromisoformat(date_match.group(1))
    except ValueError:
        raise ValueError(
            f"the heading of {version} has an invalid date {date_match.group(1)!r}"
        ) from None
    body: list[str] = []
    for line in lines[start + 1 :]:
        if line.startswith("## ") or LINK_REF_RE.match(line):
            break
        body.append(line)
    while body and not body[0].strip():
        body.pop(0)
    while body and not body[-1].strip():
        body.pop()
    if not any(BULLET_RE.match(line) for line in body):
        raise ValueError(f"the section of {version} has no '- ' bullet item")
    return "\n".join(body)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="0.1.0 or v0.1.0")
    parser.add_argument(
        "--changelog", type=Path, default=REPO / "CHANGELOG.md", help="changelog file"
    )
    parser.add_argument("--out", type=Path, help="write the notes here, not to stdout")
    args = parser.parse_args(argv)

    if not VERSION_RE.match(args.version.removeprefix("v")):
        print(f"error: {args.version!r} is not a version X.Y.Z", file=sys.stderr)
        return 2
    try:
        text = args.changelog.read_text(encoding="utf-8")
    except OSError as e:
        print(f"error: cannot read {args.changelog}: {e}", file=sys.stderr)
        return 1
    try:
        notes = extract_notes(text, args.version)
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    if args.out:
        args.out.write_text(notes + "\n", encoding="utf-8")
    else:
        sys.stdout.buffer.write((notes + "\n").encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
