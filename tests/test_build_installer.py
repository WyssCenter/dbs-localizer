# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""``scripts/build_installer.py``: MSI version mapping and RTF conversion."""

from __future__ import annotations

import pytest

from scripts import build_installer as bi


def test_msi_version_plain() -> None:
    assert bi.msi_version("1.2.3") == "1.2.3"


def test_msi_version_strips_prerelease() -> None:
    assert bi.msi_version("0.1.0-rc.1") == "0.1.0"


def test_msi_version_strips_build_metadata() -> None:
    assert bi.msi_version("1.2.3+abc.4") == "1.2.3"
    assert bi.msi_version("1.2.3-beta.2+abc") == "1.2.3"


@pytest.mark.parametrize(
    "version", ["256.0.0", "0.256.0", "0.0.65536", "1.2", "x.y.z", ""]
)
def test_msi_version_rejects(version: str) -> None:
    with pytest.raises(ValueError):
        bi.msi_version(version)


def test_msi_version_limits_accepted() -> None:
    assert bi.msi_version("255.255.65535") == "255.255.65535"


def test_rtf_frame() -> None:
    rtf = bi.license_rtf("hello")
    assert rtf.startswith("{\\rtf1")
    assert rtf.endswith("}")
    assert "hello" in rtf


def test_rtf_escapes_control_characters() -> None:
    body = bi.license_rtf("a\\b {c}")
    assert "a\\\\b \\{c\\}" in body


def test_rtf_non_ascii() -> None:
    body = bi.license_rtf("Köhler – x")
    assert "K\\u246?hler" in body
    assert "\\u8211?" in body
    assert body.isascii()


def test_rtf_astral_uses_surrogates() -> None:
    assert "\\u-10179?\\u-8704?" in bi.license_rtf("\U0001f600")


def test_rtf_newlines_become_par() -> None:
    body = bi.license_rtf("one\r\ntwo\nthree")
    assert body.count("\\par") == 2
