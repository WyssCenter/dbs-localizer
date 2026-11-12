# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Check that a release tag `vX.Y.Z` names the version in Cargo.toml and pyproject.toml.

Usage: python scripts/check_release_version.py --tag v1.2.3 [--ref refs/tags/v1.2.3]

Without a tag (a `workflow_dispatch` run on a branch) only the two files are compared.
Exit 1 on any mismatch, so a release can never ship a binary whose `--version` disagrees
with its tag.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
TAG_RE = re.compile(r"^v(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.\-]+)?)$")


def workspace_version() -> str:
    data = tomllib.loads((REPO / "Cargo.toml").read_text(encoding="utf-8"))
    return str(data["workspace"]["package"]["version"])


def python_version() -> str:
    data = tomllib.loads((REPO / "pyproject.toml").read_text(encoding="utf-8"))
    return str(data["project"]["version"])


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="", help="the git tag, e.g. v1.2.3")
    parser.add_argument(
        "--ref", default="", help="the git ref; tags are refs/tags/<tag>"
    )
    args = parser.parse_args(argv)

    cargo = workspace_version()
    py = python_version()
    errors: list[str] = []
    if cargo != py:
        errors.append(
            f"Cargo.toml workspace version {cargo} != pyproject.toml version {py}"
        )

    is_tag = args.ref.startswith("refs/tags/")
    if is_tag or args.tag.startswith("v"):
        m = TAG_RE.match(args.tag)
        if not m:
            errors.append(f"tag {args.tag!r} is not of the form vX.Y.Z")
        elif m.group(1) != cargo:
            errors.append(
                f"tag {args.tag} names version {m.group(1)} but Cargo.toml says {cargo}"
            )

    if errors:
        for e in errors:
            print(f"error: {e}", file=sys.stderr)
        print(
            "Bump `version` under [workspace.package] in Cargo.toml and `version` in "
            "pyproject.toml to the tag's version, commit, then tag that commit.",
            file=sys.stderr,
        )
        return 1
    print(f"version {cargo} ok" + (f" (tag {args.tag})" if args.tag else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
