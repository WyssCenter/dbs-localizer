# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Smoke tests for package import and version."""

import tomllib
from pathlib import Path

import dbsloc


def test_version_matches_pyproject() -> None:
    pyproject = Path(__file__).resolve().parents[1] / "pyproject.toml"
    with pyproject.open("rb") as f:
        assert dbsloc.__version__ == tomllib.load(f)["project"]["version"]
