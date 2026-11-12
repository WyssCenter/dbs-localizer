# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Pytest: optional Lead-Tutor BIDS at ``HPDATA_LEADTUTOR`` (see ``test_lead_tutor_integration``)."""

from __future__ import annotations

import os
from pathlib import Path

import pytest


def discover_bids_root(candidates: list[Path]) -> Path | None:
    """
    Return directory containing ``dataset_description.json``, trying each path
    and a ``LeadTutor/`` child (common after unzipping OSF).
    """
    for c in candidates:
        if c is None:
            continue
        c = c.expanduser().resolve()
        if not c.exists():
            continue
        for r in (c, c / "LeadTutor"):
            if (r / "dataset_description.json").is_file():
                return r
    return None


def lead_tutor_root_from_env() -> Path | None:
    """``HPDATA_LEADTUTOR``, or ``None`` when it is unset."""
    env = os.environ.get("HPDATA_LEADTUTOR", "").strip()
    if not env:
        return None
    return discover_bids_root([Path(env)])


@pytest.fixture
def lead_tutor_root() -> Path:
    root = lead_tutor_root_from_env()
    if root is None:
        pytest.skip(
            "Lead-Tutor BIDS not found. Set HPDATA_LEADTUTOR to the folder "
            "that contains dataset_description.json (e.g. .../LeadTutor/LeadTutor)."
        )
    return root


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers", "integration: needs local data (HPDATA_LEADTUTOR, Lead-Tutor BIDS)"
    )
