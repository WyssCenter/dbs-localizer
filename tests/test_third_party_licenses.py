# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""``scripts/third_party_licenses.py``: dependency walk, licence-file discovery, rendering."""

from __future__ import annotations

from pathlib import Path

import pytest

from scripts import build_installer
from scripts import package_release as pr
from scripts import third_party_licenses as tpl


def pkg(
    root: Path,
    name: str,
    *,
    where: str | None = None,
    source: str | None = "registry+https://github.com/rust-lang/crates.io-index",
    license: str | None = "MIT",
    license_file: str | None = None,
    files: dict[str, str] | None = None,
) -> dict:
    directory = root / (where or f"registry/{name}-1.0.0")
    directory.mkdir(parents=True, exist_ok=True)
    for fname, text in (files or {}).items():
        (directory / fname).write_text(text, encoding="utf-8")
    return {
        "id": f"{name} 1.0.0",
        "name": name,
        "version": "1.0.0",
        "source": source,
        "license": license,
        "license_file": license_file,
        "repository": f"https://example.org/{name}",
        "manifest_path": str(directory / "Cargo.toml"),
    }


def node(name: str, deps: dict[str, list[str | None]]) -> dict:
    return {
        "id": f"{name} 1.0.0",
        "deps": [
            {
                "pkg": f"{dep} 1.0.0",
                "dep_kinds": [{"kind": k, "target": None} for k in kinds],
            }
            for dep, kinds in deps.items()
        ],
    }


@pytest.fixture
def fake(tmp_path: Path) -> tuple[dict, Path]:
    packages = [
        pkg(
            tmp_path, "dbsloc-app", where="crates/dbsloc-app", source=None, license=None
        ),
        pkg(
            tmp_path, "dbsloc-cli", where="crates/dbsloc-cli", source=None, license=None
        ),
        pkg(
            tmp_path,
            "dbsloc-core",
            where="crates/dbsloc-core",
            source=None,
            license=None,
        ),
        pkg(
            tmp_path,
            "engine",
            where="fastwarp/crates/engine",
            source=None,
            license="Apache-2.0",
            files={"LICENSE": "apache text"},
        ),
        pkg(tmp_path, "serde", files={"LICENSE-MIT": "mit text"}),
        pkg(tmp_path, "transitive", files={"LICENSE-MIT": "mit text"}),
        pkg(tmp_path, "cc", files={"LICENSE": "cc text"}),
        pkg(tmp_path, "proptest", files={"LICENSE": "proptest text"}),
    ]
    nodes = [
        node("dbsloc-app", {"dbsloc-core": [None], "engine": [None], "cc": ["build"]}),
        node("dbsloc-cli", {"serde": [None, "build"], "proptest": ["dev"]}),
        node("dbsloc-core", {"transitive": [None]}),
        node("engine", {}),
        node("serde", {}),
        node("transitive", {}),
        node("cc", {"serde": [None]}),
        node("proptest", {}),
    ]
    metadata = {
        "packages": packages,
        "workspace_members": [
            "dbsloc-app 1.0.0",
            "dbsloc-cli 1.0.0",
            "dbsloc-core 1.0.0",
        ],
        "resolve": {"nodes": nodes},
    }
    return metadata, tmp_path


def test_walk_follows_normal_edges_only(fake: tuple[dict, Path]) -> None:
    metadata, root = fake
    bundle = tpl.collect(metadata, root, "x86_64-unknown-linux-gnu")
    names = {p.name for p in bundle.packages}
    assert {"serde", "transitive", "engine"} <= names
    assert "cc" not in names
    assert "proptest" not in names


def test_workspace_gpl_crates_excluded_engine_included(fake: tuple[dict, Path]) -> None:
    metadata, root = fake
    bundle = tpl.collect(metadata, root, "t")
    names = {p.name for p in bundle.packages}
    assert not names & {"dbsloc-app", "dbsloc-cli", "dbsloc-core"}
    assert [p.name for p in bundle.engine] == ["engine"]
    assert "engine" not in {p.name for p in bundle.third_party}


def test_licence_file_discovery(tmp_path: Path) -> None:
    d = tmp_path / "p"
    d.mkdir()
    for name in (
        "LICENSE-MIT",
        "licence.txt",
        "COPYING",
        "NOTICE",
        "UNLICENSE",
        "Copyright.md",
        "README.md",
        "Cargo.toml",
    ):
        (d / name).write_text(f"text of {name}", encoding="utf-8")
    (d / "sub").mkdir()
    (d / "sub" / "LICENSE").write_text("nested", encoding="utf-8")
    (d / "LICENSE-BAD").write_bytes(b"caf\xe9 ok")
    found = dict(tpl.licence_files(d))
    assert set(found) == {
        "LICENSE-MIT",
        "licence.txt",
        "COPYING",
        "NOTICE",
        "UNLICENSE",
        "Copyright.md",
        "LICENSE-BAD",
    }
    assert "�" in found["LICENSE-BAD"]


def test_licence_directory_is_read(tmp_path: Path) -> None:
    d = tmp_path / "p"
    (d / "LICENSES").mkdir(parents=True)
    (d / "LICENSES" / "MIT.txt").write_text("mit in dir", encoding="utf-8")
    (d / "LICENSES" / "deeper").mkdir()
    (d / "LICENSES" / "deeper" / "X.txt").write_text("too deep", encoding="utf-8")
    (d / "licenses").mkdir(exist_ok=True)
    found = dict(tpl.licence_files(d))
    assert found == {"LICENSES/MIT.txt": "mit in dir"}


def test_nested_font_style_files_are_found_when_nothing_else(tmp_path: Path) -> None:
    d = tmp_path / "p"
    (d / "fonts").mkdir(parents=True)
    (d / "fonts" / "OFL.txt").write_text("ofl text", encoding="utf-8")
    (d / "fonts" / "UFL.txt").write_text("ufl text", encoding="utf-8")
    (d / "fonts" / "Hack-Regular.txt").write_text("hack text", encoding="utf-8")
    (d / "fonts" / "Hack-Regular.ttf").write_bytes(b"x")
    (d / "fonts" / "notes.txt").write_text("not a licence", encoding="utf-8")
    (d / "fonts" / "icons-license.txt").write_text("icons", encoding="utf-8")
    (d / "a" / "b" / "c" / "d").mkdir(parents=True)
    (d / "a" / "b" / "c" / "d" / "LICENSE").write_text("too deep", encoding="utf-8")
    (d / "a" / "b" / "LICENSE").write_text("depth three", encoding="utf-8")
    found = dict(tpl.licence_files(d))
    assert found == {
        "fonts/OFL.txt": "ofl text",
        "fonts/UFL.txt": "ufl text",
        "fonts/Hack-Regular.txt": "hack text",
        "fonts/icons-license.txt": "icons",
        "a/b/LICENSE": "depth three",
    }


def test_deep_search_skipped_when_top_level_has_a_file(tmp_path: Path) -> None:
    d = tmp_path / "p"
    (d / "fonts").mkdir(parents=True)
    (d / "LICENSE").write_text("top", encoding="utf-8")
    (d / "fonts" / "OFL.txt").write_text("ofl", encoding="utf-8")
    assert dict(tpl.licence_files(d)) == {"LICENSE": "top"}


def test_oversize_file_is_truncated_with_notice(tmp_path: Path) -> None:
    d = tmp_path / "p"
    d.mkdir()
    (d / "LICENSE-BIG").write_text("x" * (tpl.MAX_FILE_BYTES + 5), encoding="utf-8")
    (d / "LICENSE-EXACT").write_text("y" * tpl.MAX_FILE_BYTES, encoding="utf-8")
    found = dict(tpl.licence_files(d))
    big = found["LICENSE-BIG"]
    assert big.startswith("x" * tpl.MAX_FILE_BYTES)
    assert "x" * (tpl.MAX_FILE_BYTES + 1) not in big
    assert "Truncated: LICENSE-BIG" in big
    assert found["LICENSE-EXACT"] == "y" * tpl.MAX_FILE_BYTES


def bare_metadata(root: Path, expression: str | None = "MIT") -> dict:
    return {
        "packages": [
            pkg(root, "dbsloc-app", where="crates/a", source=None, license=None),
            pkg(root, "bare", license=expression),
        ],
        "workspace_members": ["dbsloc-app 1.0.0"],
        "resolve": {"nodes": [node("dbsloc-app", {"bare": [None]}), node("bare", {})]},
    }


def test_no_licence_file_allow_listed_listed_and_counted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(tpl, "NO_LICENCE_FILE_ALLOWED", frozenset({"bare"}))
    metadata = bare_metadata(tmp_path, "MIT OR Apache-2.0")
    text = tpl.render(tpl.collect(metadata, tmp_path, "t"))
    assert "The published package contains no licence file" in text
    assert "https://example.org/bare" in text
    assert (
        "Packages without a licence file in the published package: 1 (bare 1.0.0)"
        in text
    )
    assert "MIT OR Apache-2.0" in text
    assert "is reproduced in this document" not in text


def test_no_licence_file_not_allow_listed_is_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(tpl, "NO_LICENCE_FILE_ALLOWED", frozenset())
    with pytest.raises(tpl.LicenceError, match="bare 1.0.0") as info:
        tpl.collect(bare_metadata(tmp_path), tmp_path, "t")
    assert "NO_LICENCE_FILE_ALLOWED" in str(info.value)


def test_cli_fails_for_package_without_licence_file(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(tpl, "NO_LICENCE_FILE_ALLOWED", frozenset())
    metadata = bare_metadata(tmp_path)
    monkeypatch.setattr(tpl, "host_triple", lambda: "t")
    monkeypatch.setattr(tpl, "cargo_metadata", lambda *_a: metadata)
    monkeypatch.setattr(tpl, "REPO", tmp_path)
    out = tmp_path / "out" / "bundle.md"
    assert tpl.main(["--out", str(out)]) == 1
    assert "bare" in capsys.readouterr().err
    assert not out.exists()


def test_standard_text_cross_reference_present(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(tpl, "NO_LICENCE_FILE_ALLOWED", frozenset({"bare"}))
    metadata = bare_metadata(tmp_path, "MIT")
    metadata["packages"].append(pkg(tmp_path, "donor", files={"LICENSE": "mit text"}))
    metadata["resolve"]["nodes"][0] = node(
        "dbsloc-app", {"bare": [None], "donor": [None]}
    )
    metadata["resolve"]["nodes"].append(node("donor", {}))
    text = tpl.render(tpl.collect(metadata, tmp_path, "t"))
    assert (
        "The standard text of MIT is reproduced in this document under donor 1.0.0."
        in text
    )
    assert "mit text" in text


def test_standard_text_cross_reference_absent_without_donor(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(tpl, "NO_LICENCE_FILE_ALLOWED", frozenset({"bare"}))
    metadata = bare_metadata(tmp_path, "MIT")
    metadata["packages"].append(
        pkg(tmp_path, "other", license="Apache-2.0", files={"LICENSE": "apache"})
    )
    metadata["packages"].append(
        pkg(
            tmp_path,
            "two",
            license="MIT",
            files={"LICENSE-MIT": "a", "LICENSE-APACHE": "b"},
        )
    )
    metadata["resolve"]["nodes"][0] = node(
        "dbsloc-app", {"bare": [None], "other": [None], "two": [None]}
    )
    metadata["resolve"]["nodes"] += [node("other", {}), node("two", {})]
    text = tpl.render(tpl.collect(metadata, tmp_path, "t"))
    assert "The published package contains no licence file" in text
    assert "The standard text of MIT" not in text


def test_identical_texts_are_printed_once(fake: tuple[dict, Path]) -> None:
    metadata, root = fake
    (root / "registry/transitive-1.0.0/LICENSE-MIT").write_bytes(b"mit text\r\n")
    (root / "registry/serde-1.0.0/LICENSE-MIT").write_bytes(b"mit text\n")
    text = tpl.render(tpl.collect(metadata, root, "t"))
    assert text.count("mit text") == 1
    assert "Used by: serde 1.0.0, transitive 1.0.0" in text
    assert "apache text" in text


def test_render_contains_name_version_expression_and_text(
    fake: tuple[dict, Path],
) -> None:
    metadata, root = fake
    text = tpl.render(tpl.collect(metadata, root, "aarch64-apple-darwin"))
    assert "### serde 1.0.0" in text
    assert "- Licence: MIT" in text
    assert "https://example.org/serde" in text
    assert "aarch64-apple-darwin" in text
    assert "| MIT | 2 |" in text
    assert "dbs-localizer" in text


def test_package_without_expression_or_file_is_error(tmp_path: Path) -> None:
    metadata = {
        "packages": [
            pkg(tmp_path, "dbsloc-app", where="crates/a", source=None, license=None),
            pkg(tmp_path, "nolicence", license=None),
        ],
        "workspace_members": ["dbsloc-app 1.0.0"],
        "resolve": {
            "nodes": [node("dbsloc-app", {"nolicence": [None]}), node("nolicence", {})]
        },
    }
    with pytest.raises(tpl.LicenceError, match="nolicence"):
        tpl.collect(metadata, tmp_path, "t")


def test_license_file_field_is_used(tmp_path: Path) -> None:
    metadata = {
        "packages": [
            pkg(tmp_path, "dbsloc-app", where="crates/a", source=None, license=None),
            pkg(
                tmp_path,
                "custom",
                license=None,
                license_file="TERMS.txt",
                files={"TERMS.txt": "custom terms"},
            ),
        ],
        "workspace_members": ["dbsloc-app 1.0.0"],
        "resolve": {
            "nodes": [node("dbsloc-app", {"custom": [None]}), node("custom", {})]
        },
    }
    text = tpl.render(tpl.collect(metadata, tmp_path, "t"))
    assert "custom terms" in text
    assert tpl.FILE_ONLY in text


def test_real_metadata_for_host() -> None:
    target = tpl.host_triple()
    try:
        metadata = tpl.cargo_metadata(tpl.REPO, target, offline=True)
    except tpl.LicenceError:
        pytest.skip("cargo metadata is not available offline")
    bundle = tpl.collect(metadata, tpl.REPO, target)
    names = {p.name for p in bundle.packages}
    assert len(bundle.packages) > 100
    assert {"egui", "serde"} <= names
    assert not any(n.startswith("dbsloc-app") for n in names)
    without_text = {p.name for p in bundle.packages if not p.files}
    assert without_text <= tpl.NO_LICENCE_FILE_ALLOWED, sorted(
        without_text - tpl.NO_LICENCE_FILE_ALLOWED
    )
    assert "epaint_default_fonts" not in without_text


def test_packaging_scripts_carry_the_bundle() -> None:
    assert pr.THIRD_PARTY_NAME == "THIRD_PARTY_LICENSES.md"
    assert pr.THIRD_PARTY_NAME in build_installer.REQUIRED_PAYLOAD
    assert pr.THIRD_PARTY_NAME in pr.readme_text("win")
    assert pr.THIRD_PARTY_NAME in pr.readme_text("linux")
