# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""``scripts/release_notes.py`` and the changelog rotation of ``scripts/bump_version.py``."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from scripts import bump_version as bv
from scripts import release_notes as rn

SCRIPT = Path(rn.__file__)
URL = "https://example.org/repo"

CHANGELOG = """# Changelog

Intro text.

## [Unreleased]

### Added

- Foo.

## [0.2.0] - 2026-10-23

### Added

- Two.

### Fixed

- Two fix.

## [0.1.0] - 2026-09-01

### Added

- One.

[unreleased]: https://example.org/repo/compare/v0.2.0...HEAD
[0.2.0]: https://example.org/repo/compare/v0.1.0...v0.2.0
[0.1.0]: https://example.org/repo/releases/tag/v0.1.0
"""


def run_cli(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )


def write_changelog(tmp_path: Path) -> Path:
    changelog = tmp_path / "CHANGELOG.md"
    changelog.write_text(CHANGELOG, encoding="utf-8")
    return changelog


def test_extract_stops_at_next_heading() -> None:
    notes = rn.extract_notes(CHANGELOG, "0.2.0")
    assert notes == "### Added\n\n- Two.\n\n### Fixed\n\n- Two fix."


def test_extract_stops_at_link_references() -> None:
    assert rn.extract_notes(CHANGELOG, "0.1.0") == "### Added\n\n- One."


def test_extract_accepts_v_prefix() -> None:
    assert rn.extract_notes(CHANGELOG, "v0.1.0") == "### Added\n\n- One."


def test_extract_missing_section() -> None:
    with pytest.raises(ValueError, match="no section"):
        rn.extract_notes(CHANGELOG, "0.3.0")


def test_extract_missing_date() -> None:
    with pytest.raises(ValueError, match="no date"):
        rn.extract_notes("## [0.1.0]\n\n- A.\n", "0.1.0")


def test_extract_invalid_date() -> None:
    with pytest.raises(ValueError, match="invalid date"):
        rn.extract_notes("## [0.1.0] - 2026-13-45\n\n- A.\n", "0.1.0")


def test_extract_empty_body() -> None:
    text = "## [0.1.0] - 2026-10-23\n\n## [0.0.1] - 2026-01-01\n\n- A.\n"
    with pytest.raises(ValueError, match="bullet"):
        rn.extract_notes(text, "0.1.0")


def test_extract_body_without_bullet() -> None:
    with pytest.raises(ValueError, match="bullet"):
        rn.extract_notes("## [0.1.0] - 2026-10-23\n\nJust prose.\n", "0.1.0")


def test_extract_duplicate_heading() -> None:
    text = "## [0.1.0] - 2026-10-23\n\n- A.\n\n## [0.1.0] - 2026-10-24\n\n- B.\n"
    with pytest.raises(ValueError, match="2 sections"):
        rn.extract_notes(text, "0.1.0")


def test_cli_writes_out_file(tmp_path: Path) -> None:
    out = tmp_path / "notes.md"
    r = run_cli(
        "v0.1.0", "--changelog", str(write_changelog(tmp_path)), "--out", str(out)
    )
    assert r.returncode == 0, r.stderr
    assert out.read_text(encoding="utf-8") == "### Added\n\n- One.\n"


def test_cli_stdout(tmp_path: Path) -> None:
    r = run_cli("0.1.0", "--changelog", str(write_changelog(tmp_path)))
    assert r.returncode == 0
    assert r.stdout.strip() == "### Added\n\n- One."


def test_cli_missing_section_exit_1(tmp_path: Path) -> None:
    r = run_cli("9.9.9", "--changelog", str(write_changelog(tmp_path)))
    assert r.returncode == 1
    assert r.stderr.startswith("error:")


def test_cli_missing_file_exit_1(tmp_path: Path) -> None:
    r = run_cli("0.1.0", "--changelog", str(tmp_path / "none.md"))
    assert r.returncode == 1
    assert r.stderr.startswith("error:")


def test_cli_bad_version_exit_2(tmp_path: Path) -> None:
    r = run_cli("banana", "--changelog", str(tmp_path / "none.md"))
    assert r.returncode == 2


def test_rotate_with_previous_version() -> None:
    out = bv.rotate_changelog(CHANGELOG, "0.3.0", "2026-11-01", URL)
    assert "## [Unreleased]\n\n## [0.3.0] - 2026-11-01\n\n### Added\n\n- Foo.\n" in out
    assert (
        f"[unreleased]: {URL}/compare/v0.3.0...HEAD\n"
        f"[0.3.0]: {URL}/compare/v0.2.0...v0.3.0\n"
        "[0.2.0]: " in out
    )
    assert out.count("[unreleased]:") == 1
    assert out.endswith("\n") and not out.endswith("\n\n")
    assert rn.extract_notes(out, "0.3.0") == "### Added\n\n- Foo."


def test_rotate_leaves_empty_unreleased_on_top() -> None:
    out = bv.rotate_changelog(CHANGELOG, "0.3.0", "2026-11-01", URL)
    assert out.index("## [Unreleased]") < out.index("## [0.3.0]")
    with pytest.raises(ValueError, match="nothing under"):
        bv.rotate_changelog(out, "0.4.0", "2026-11-02", URL)


def test_rotate_first_release() -> None:
    text = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Foo.\n\n[unreleased]: x\n"
    out = bv.rotate_changelog(text, "0.1.0", "2026-10-23", URL)
    assert f"[0.1.0]: {URL}/releases/tag/v0.1.0\n" in out
    assert f"[unreleased]: {URL}/compare/v0.1.0...HEAD\n" in out
    assert "[unreleased]: x" not in out


def test_rotate_without_link_block() -> None:
    text = "# Changelog\n\n## [Unreleased]\n\n- Foo.\n\n\n"
    out = bv.rotate_changelog(text, "0.1.0", "2026-10-23", URL)
    assert out == (
        "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-10-23\n\n- Foo.\n\n"
        f"[unreleased]: {URL}/compare/v0.1.0...HEAD\n"
        f"[0.1.0]: {URL}/releases/tag/v0.1.0\n"
    )


def test_rotate_unreleased_heading_case_insensitive() -> None:
    out = bv.rotate_changelog("## [UNRELEASED]\n\n- Foo.\n", "0.1.0", "2026-10-23", URL)
    assert "## [Unreleased]\n\n## [0.1.0] - 2026-10-23" in out


def test_rotate_empty_unreleased_fails() -> None:
    text = "## [Unreleased]\n\n### Added\n\n## [0.1.0] - 2026-10-23\n\n- One.\n"
    with pytest.raises(ValueError, match="nothing under"):
        bv.rotate_changelog(text, "0.2.0", "2026-11-01", URL)


def test_rotate_missing_unreleased_fails() -> None:
    text = "## [0.1.0] - 2026-10-23\n\n- One.\n"
    with pytest.raises(ValueError, match="Unreleased"):
        bv.rotate_changelog(text, "0.2.0", "2026-11-01", URL)


def test_rotate_existing_version_fails() -> None:
    with pytest.raises(ValueError, match="already has"):
        bv.rotate_changelog(CHANGELOG, "0.2.0", "2026-11-01", URL)
