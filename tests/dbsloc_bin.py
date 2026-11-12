# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

_REPO = Path(__file__).resolve().parent.parent


def find_dbsloc() -> Path | None:
    env = os.environ.get("DBSLOC_BIN", "").strip()
    if env and Path(env).is_file():
        return Path(env)
    name = "dbsloc.exe" if sys.platform == "win32" else "dbsloc"
    base = Path(os.environ.get("CARGO_TARGET_DIR", _REPO / "target"))
    for profile in ("release", "debug"):
        cand = base / profile / name
        if cand.is_file():
            return cand
    return None


def dbsloc_or_skip() -> str:
    exe = find_dbsloc()
    if exe is None:
        pytest.skip(
            "dbsloc binary not found: set DBSLOC_BIN or run "
            "`cargo build -p dbsloc-cli --release`"
        )
    return str(exe)
