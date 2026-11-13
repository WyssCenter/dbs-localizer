# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Every release package states where its corresponding source code is (GPL version 3, section 6)."""

from __future__ import annotations

from pathlib import Path

from scripts import build_deb as bd
from scripts import build_installer as bi
from scripts import package_release as pr

REPO_URL = "https://github.com/WyssCenter/dbs-localizer"
REPO = Path(__file__).resolve().parents[1]


def test_repository_address_is_the_same_in_every_script() -> None:
    assert pr.REPO_URL == REPO_URL
    assert bd.REPO_URL == REPO_URL
    assert bi.REPO_URL == REPO_URL


def test_readme_states_licence_and_source_tag() -> None:
    for plat in ("win", "linux", "mac"):
        text = pr.readme_text(plat, "1.2.3")
        assert "Licence and source code" in text
        assert "GNU General Public License" in text
        assert "version 3 only" in text
        assert f"{REPO_URL}/tree/v1.2.3" in text
        assert "NOTICE" in text and "LICENSES/" in text and pr.THIRD_PARTY_NAME in text
        assert "GNU Affero General Public" in text
        assert "LICENSES/AGPL-3.0-only.txt" in text


def test_packages_ship_the_agpl_text_of_the_pacer_component() -> None:
    assert "AGPL-3.0-only.txt" in pr.REQUIRED_LICENCE_TEXTS
    for name in pr.REQUIRED_LICENCE_TEXTS:
        assert (REPO / "LICENSES" / name).is_file(), name
    text = bd.copyright_text("0.1.0")
    assert "License: GPL-3.0-only and AGPL-3.0-only\n" in text
    assert "\n\nLicense: AGPL-3.0-only\n" in text
    assert "LICENSES/AGPL-3.0-only.txt" in text


def test_readme_defaults_to_workspace_version() -> None:
    version = pr.workspace_version()
    assert f"{REPO_URL}/tree/v{version}" in pr.readme_text("linux")


def _layout(root: Path) -> pr.Layout:
    return pr.Layout("linux", root, root / "bin", root / "resources", root / "a", [])


def test_package_self_check_fails_without_source_statement(tmp_path: Path) -> None:
    assert not pr.check_readme_source(_layout(tmp_path)).ok
    (tmp_path / pr.README_NAME).write_text("Wyss DBS Localizer\n", encoding="utf-8")
    assert not pr.check_readme_source(_layout(tmp_path)).ok


def test_package_self_check_passes_with_source_statement(tmp_path: Path) -> None:
    (tmp_path / pr.README_NAME).write_text(
        pr.readme_text("linux", "1.2.3"), encoding="utf-8"
    )
    assert pr.check_readme_source(_layout(tmp_path)).ok


def test_deb_control_has_homepage() -> None:
    assert f"Homepage: {REPO_URL}\n" in bd.control_text("0.1.0", 1, "2.39")


def test_deb_copyright_is_machine_readable() -> None:
    text = bd.copyright_text("0.1.0-rc.1")
    header, _, rest = text.partition("\n\n")
    fields = dict(
        ln.split(": ", 1) for ln in header.splitlines() if ln and not ln.startswith(" ")
    )
    assert fields["Format"].startswith("https://www.debian.org/doc/packaging-manuals/")
    assert fields["Upstream-Name"] == "Wyss DBS Localizer"
    assert fields["Source"] == REPO_URL
    assert f"{REPO_URL}/tree/v0.1.0-rc.1" in header
    assert "Files: *" in rest
    assert "Copyright: 2026 Wyss Center for Bio and Neuroengineering" in rest
    assert "License: GPL-3.0-only" in rest
    assert "/opt/dbs-localizer/LICENSE" in rest


def test_installer_help_links_use_repository_variable() -> None:
    wxs = (REPO / "packaging" / "wix" / "Product.wxs").read_text(encoding="utf-8")
    assert 'Id="ARPHELPLINK" Value="$(var.RepoUrl)"' in wxs
    assert 'Id="ARPURLINFOABOUT" Value="https://www.wysscenter.ch"' in wxs
    source = (REPO / "scripts" / "build_installer.py").read_text(encoding="utf-8")
    assert 'f"RepoUrl={REPO_URL}"' in source
