# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

from __future__ import annotations

import platform
from pathlib import Path

import pytest

from dbsloc import bundled_bin


def test_ants_dir_environment_wins(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = (
        "antsRegistration.exe" if platform.system() == "Windows" else "antsRegistration"
    )
    exe = tmp_path / name
    exe.write_bytes(b"")
    exe.chmod(0o755)
    monkeypatch.setenv("DBSLOC_ANTS_DIR", str(tmp_path))
    assert bundled_bin.which_ants_binary("antsRegistration") == str(exe.resolve())


@pytest.mark.parametrize(
    ("system", "machine", "expected"),
    [
        ("Linux", "x86_64", "linux"),
        ("Linux", "aarch64", None),
        ("Darwin", "arm64", "mac"),
        ("Darwin", "x86_64", None),
        ("Windows", "AMD64", "win"),
        ("FreeBSD", "amd64", None),
    ],
)
def test_bundle_is_only_offered_for_supported_cpus(
    monkeypatch: pytest.MonkeyPatch, system: str, machine: str, expected: str | None
) -> None:
    monkeypatch.setattr(bundled_bin.platform, "system", lambda: system)
    monkeypatch.setattr(bundled_bin.platform, "machine", lambda: machine)
    assert bundled_bin._platform_dir() == expected
