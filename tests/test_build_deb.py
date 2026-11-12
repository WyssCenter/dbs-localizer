# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""``scripts/build_deb.py``: version mapping, control file, soname mapping, desktop entry."""

from __future__ import annotations

import pytest

from scripts import build_deb as bd


def test_deb_version_plain() -> None:
    assert bd.deb_version("0.1.0") == "0.1.0"


def test_deb_version_prerelease() -> None:
    assert bd.deb_version("0.1.0-rc.1") == "0.1.0~rc.1"


def test_deb_version_build_metadata() -> None:
    assert bd.deb_version("0.1.0+build5") == "0.1.0+build5"
    assert bd.deb_version("1.0.0-rc.1+b5") == "1.0.0~rc.1+b5"


def test_deb_version_rejects_garbage() -> None:
    with pytest.raises(bd.DebError):
        bd.deb_version("v1.0")
    with pytest.raises(bd.DebError):
        bd.deb_version("1.0 beta")


def test_control_fields() -> None:
    text = bd.control_text("0.1.0", 1234, "2.39")
    fields = dict(
        ln.split(": ", 1) for ln in text.splitlines() if ln and not ln.startswith(" ")
    )
    assert fields["Package"] == "dbs-localizer"
    assert fields["Version"] == "0.1.0"
    assert fields["Architecture"] == "amd64"
    assert fields["Section"] == "science"
    assert fields["Installed-Size"] == "1234"
    assert fields["Maintainer"] == "Richard Köhler <richard.koehler@wysscenter.ch>"
    assert fields["Recommends"] == "mesa-vulkan-drivers"
    assert fields["Homepage"] == "https://github.com/WyssCenter/dbs-localizer"
    assert text.endswith("\n")


def test_control_depends() -> None:
    depends = bd.control_text("0.1.0", 1, "2.39").split("Depends: ")[1].split("\n")[0]
    assert depends.startswith("libc6 (>= 2.39), ")
    for want in (
        "libgtk-3-0t64 | libgtk-3-0",
        "libvulkan1",
        "libxkbcommon0",
        "libfontconfig1",
    ):
        assert want in depends


def test_control_unknown_glibc_drops_constraint() -> None:
    depends = (
        bd.control_text("0.1.0", 1, "unknown").split("Depends: ")[1].split("\n")[0]
    )
    assert depends.startswith("libc6, ")


def test_control_long_description_is_continued() -> None:
    lines = bd.control_text("0.1.0", 1, "2.39").splitlines()
    start = next(i for i, ln in enumerate(lines) if ln.startswith("Description: "))
    assert all(ln.startswith(" ") for ln in lines[start + 1 :])


def test_soname_known() -> None:
    assert bd.package_for_soname("libc.so.6") == "libc6"
    assert bd.package_for_soname("ld-linux-x86-64.so.2") == "libc6"
    assert bd.package_for_soname("libgcc_s.so.1") == "libgcc-s1"
    assert bd.package_for_soname("libvulkan.so.1") == "libvulkan1"
    assert bd.package_for_soname("libgtk-3.so.0") == "libgtk-3-0t64 | libgtk-3-0"


def test_soname_unknown_is_error() -> None:
    with pytest.raises(bd.DebError, match="libmystery.so.3"):
        bd.package_for_soname("libmystery.so.3")


def test_parse_needed() -> None:
    out = (
        "Dynamic section at offset 0x1 contains 2 entries:\n"
        " 0x0000000000000001 (NEEDED)             Shared library: [libm.so.6]\n"
        " 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]\n"
        " 0x000000000000000c (INIT)               0x1000\n"
    )
    assert bd.parse_needed(out) == ["libm.so.6", "libc.so.6"]


def test_desktop_entry_fields() -> None:
    text = bd.desktop_entry("/usr/bin/dbs-localizer")
    fields = dict(ln.split("=", 1) for ln in text.splitlines() if "=" in ln)
    assert text.startswith("[Desktop Entry]\n")
    assert fields["Type"] == "Application"
    assert fields["Name"] == "DBS Localizer"
    assert fields["Exec"] == "/usr/bin/dbs-localizer"
    assert fields["Icon"] == "dbs-localizer"
    assert fields["Terminal"] == "false"
    assert fields["Categories"] == "Science;MedicalSoftware;Graphics;"
    assert fields["StartupWMClass"] == "dbsloc_app"
    assert "@EXEC@" not in text


def test_desktop_entry_quotes_paths_with_spaces() -> None:
    assert 'Exec="/home/a b/.local/bin/dbs-localizer"' in bd.desktop_entry(
        "/home/a b/.local/bin/dbs-localizer"
    )
