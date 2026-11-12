# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Electrode model catalogue (`specs/09_electrode_models.md`)."""

from dbsloc.electrodes.registry import get_elspec, list_models
from dbsloc.electrodes.spec import ElectrodeSpec

__all__ = ["ElectrodeSpec", "get_elspec", "list_models"]
