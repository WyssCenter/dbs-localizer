# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Wyss DBS Localizer Python package: Lead-DBS–class pipeline (written from the specifications)."""

__version__ = "0.1.0"

from dbsloc.bundled_bin import ants_binary_or_error, which_ants_binary

__all__ = ["__version__", "ants_binary_or_error", "which_ants_binary"]
