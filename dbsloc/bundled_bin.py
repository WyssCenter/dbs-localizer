# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Find and use bundled ANTs binaries. Prefer bundled over system PATH."""

from __future__ import annotations

import os
import platform
import shutil
from pathlib import Path

_SUPPORTED_ARCH: dict[str, tuple[str, frozenset[str] | None]] = {
    "linux": ("linux", frozenset({"x86_64", "amd64"})),
    "darwin": ("mac", frozenset({"arm64", "aarch64"})),
    "windows": ("win", None),
}


def _platform_dir() -> str | None:
    entry = _SUPPORTED_ARCH.get(platform.system().lower())
    if entry is None:
        return None
    name, arches = entry
    if arches is not None and platform.machine().lower() not in arches:
        return None
    return name


def _candidates(name: str) -> list[str]:
    if platform.system().lower() == "windows":
        return [f"{name}.exe", name]
    return [name]


def _in_dir(directory: Path, name: str) -> Path | None:
    if not directory.is_dir():
        return None
    for cand in _candidates(name):
        path = directory / cand
        if path.is_file() and os.access(path, os.X_OK):
            return path
    return None


def _bundled_binary_path(name: str) -> Path | None:
    """Get path to bundled binary, or None if not available."""
    env_dir = os.environ.get("DBSLOC_ANTS_DIR", "").strip()
    if env_dir:
        found = _in_dir(Path(env_dir), name)
        if found is not None:
            return found
    plat = _platform_dir()
    if plat is None:
        return None
    roots: list[Path] = []
    repo_env = os.environ.get("DBSLOC_REPO_ROOT", "").strip()
    if repo_env:
        roots.append(Path(repo_env))
    roots.append(Path(__file__).resolve().parent.parent)
    for root in roots:
        found = _in_dir(root / "resources" / "ants" / plat, name)
        if found is not None:
            return found
    return None


def which_ants_binary(name: str) -> str | None:
    """
    Find ANTs binary. Prefer bundled, fall back to PATH.

    Returns absolute path string if found, None otherwise.
    """
    bundled = _bundled_binary_path(name)
    if bundled is not None:
        return str(bundled.resolve())
    for cand in _candidates(name):
        path = shutil.which(cand)
        if path is not None:
            return path
    return None


def ants_binary_or_error(name: str) -> str:
    """
    Get path to ANTs binary or return error message.

    Returns path string if found, else "error: ..." message.
    """
    path = which_ants_binary(name)
    if path is None:
        return f"error: {name} not found (bundled or PATH)"
    return path


__all__ = [
    "ants_binary_or_error",
    "which_ants_binary",
]
