# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Smoke tests for package import and version."""

import dbsloc


def test_version_defined() -> None:
    assert dbsloc.__version__ == "0.1.0"
