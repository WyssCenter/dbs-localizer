# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Set the release version everywhere it is recorded.

Usage: python scripts/bump_version.py 0.2.0

Turns the `## [Unreleased]` section of CHANGELOG.md into `## [0.2.0] - <today>`
(leaving a new empty Unreleased section and updating the link references; stops without
touching any file when nothing is listed under Unreleased), rewrites `version`
under [workspace.package] in Cargo.toml and `version` in pyproject.toml, refreshes
Cargo.lock (`cargo update --workspace`) and uv.lock (`uv lock`), checks that the release
notes can be extracted, and prints the git commands that make the release. It never commits.
"""

from __future__ import annotations

import datetime
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.\-]+)?$")
REPO_URL = "https://github.com/WyssCenter/dbs-localizer"
UNRELEASED_RE = re.compile(r"^##\s+\[unreleased\]\s*$", re.IGNORECASE)
HEADING_RE = re.compile(r"^##\s+\[([^\]]+)\]")
LINK_REF_RE = re.compile(r"^\[([^\]]+)\]: ")
BULLET_RE = re.compile(r"^\s*[-*] \S")


def replace_once(path: Path, pattern: str, repl: str) -> None:
    text = path.read_text(encoding="utf-8")
    new, n = re.subn(pattern, repl, text, count=1, flags=re.MULTILINE)
    if n != 1:
        raise SystemExit(f"{path}: version line not found")
    path.write_text(new, encoding="utf-8")


def rotate_changelog(text: str, version: str, date: str, repo_url: str) -> str:
    lines = text.splitlines()
    unreleased = next(
        (i for i, line in enumerate(lines) if UNRELEASED_RE.match(line)), None
    )
    if unreleased is None:
        raise ValueError("CHANGELOG.md has no '## [Unreleased]' section")
    body: list[str] = []
    for line in lines[unreleased + 1 :]:
        if line.startswith("## ") or LINK_REF_RE.match(line):
            break
        body.append(line)
    if not any(BULLET_RE.match(line) for line in body):
        raise ValueError("nothing under [Unreleased]: list the changes first")
    previous: str | None = None
    for line in lines:
        m = HEADING_RE.match(line)
        if m and m.group(1) == version:
            raise ValueError(f"CHANGELOG.md already has a section for {version}")
    for line in lines[unreleased + 1 :]:
        m = HEADING_RE.match(line)
        if m and VERSION_RE.match(m.group(1)):
            previous = m.group(1)
            break
    lines[unreleased : unreleased + 1] = [
        "## [Unreleased]",
        "",
        f"## [{version}] - {date}",
    ]
    lines = [
        line
        for line in lines
        if not (
            (m := LINK_REF_RE.match(line))
            and m.group(1).lower() in {"unreleased", version.lower()}
        )
    ]
    if previous:
        version_ref = f"[{version}]: {repo_url}/compare/v{previous}...v{version}"
    else:
        version_ref = f"[{version}]: {repo_url}/releases/tag/v{version}"
    refs = [f"[unreleased]: {repo_url}/compare/v{version}...HEAD", version_ref]
    first_ref = next(
        (i for i, line in enumerate(lines) if LINK_REF_RE.match(line)), None
    )
    if first_ref is None:
        while lines and not lines[-1].strip():
            lines.pop()
        lines += ["", *refs]
    else:
        lines[first_ref:first_ref] = refs
    return "\n".join(lines).rstrip("\n") + "\n"


def main(argv: list[str]) -> int:
    if len(argv) != 1 or not VERSION_RE.match(argv[0]):
        print(__doc__, file=sys.stderr)
        return 2
    version = argv[0]
    changelog = REPO / "CHANGELOG.md"
    if not changelog.is_file():
        raise SystemExit(f"{changelog}: not found")
    try:
        rotated = rotate_changelog(
            changelog.read_text(encoding="utf-8"),
            version,
            datetime.date.today().isoformat(),
            REPO_URL,
        )
    except ValueError as e:
        raise SystemExit(f"{changelog}: {e}") from None
    replace_once(
        REPO / "Cargo.toml",
        r'^(\[workspace\.package\]\nversion = )"[^"]+"',
        rf'\1"{version}"',
    )
    replace_once(
        REPO / "pyproject.toml", r'^version = "[^"]+"', f'version = "{version}"'
    )
    changelog.write_text(rotated, encoding="utf-8")
    subprocess.run(
        ["cargo", "update", "--workspace", "--offline"], cwd=REPO, check=True
    )
    subprocess.run(["uv", "lock", "--offline"], cwd=REPO, check=True)
    subprocess.run(
        [
            sys.executable,
            str(REPO / "scripts" / "check_release_version.py"),
            "--tag",
            f"v{version}",
            "--ref",
            f"refs/tags/v{version}",
        ],
        cwd=REPO,
        check=True,
    )
    subprocess.run(
        [sys.executable, str(REPO / "scripts" / "release_notes.py"), version],
        cwd=REPO,
        check=True,
        stdout=subprocess.DEVNULL,
    )
    print(
        f"version set to {version}. Review the diff (CHANGELOG.md included), then:\n"
        f'  git commit -am "chore(release): v{version}"\n'
        f'  git tag -a v{version} -m "Wyss DBS Localizer v{version}"\n'
        f"  git push origin main v{version}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
