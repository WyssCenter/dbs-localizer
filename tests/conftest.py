# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import os

import pytest


def _env(name: str) -> str:
    value = os.environ.get(name)
    if value is None:
        value = os.environ.get(name.replace("FASTWARP_", "DBSLOC_", 1), "")
    return value


def _flag(name: str) -> bool:
    return _env(name).strip().lower() in {"1", "true", "yes"}


def pytest_sessionstart(session: pytest.Session) -> None:
    if _flag("FASTWARP_GPU_REQUIRE_ADAPTER"):
        import fastwarp as g

        if not g.gpu_available():
            pytest.exit("FASTWARP_GPU_REQUIRE_ADAPTER is set but no usable GPU adapter opens", 1)
    if _flag("FASTWARP_REQUIRE_ANTSPY"):
        try:
            import ants  # noqa: F401
        except ImportError as e:
            pytest.exit(f"FASTWARP_REQUIRE_ANTSPY is set but ANTsPy does not import: {e}", 1)
