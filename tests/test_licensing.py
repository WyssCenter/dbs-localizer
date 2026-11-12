# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Licence labelling: SPDX headers, ``REUSE.toml``, ``LICENSES/``, Cargo ``license`` fields.

Rules and the licence map: ``docs/LICENSING.md``.
"""

from __future__ import annotations

import re
import subprocess
import tomllib
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

ROOT = Path(__file__).resolve().parents[1]
GPUREG = "fastwarp"

APACHE_CRATES = frozenset(
    {
        "fastwarp-core",
        "fastwarp-niftiio",
        "fastwarp-matfile",
        "fastwarp-reg",
        "fastwarp-gpu",
    }
)
GPUREG_CRATES = APACHE_CRATES | {"fastwarp-py"}
APACHE = "Apache-2.0"
GPL = "GPL-3.0-only"
SOURCE_SUFFIXES = frozenset({".rs", ".wgsl", ".py", ".sh", ".cmd"})
HEADER_LINES = 30
ITK_COPYRIGHT = "1999-2019 Insight Software Consortium"
NUMFOCUS_COPYRIGHT = "2020-present NumFOCUS"
WYSS_COPYRIGHT = "2026 Wyss Center for Bio and Neuroengineering"
ANTS_COPYRIGHT = "ANTs developers"
APACHE_NOTICE = "Licensed under the Apache License, Version 2.0"
COMMENT_MARKERS = ("//", "#", "@REM", "REM", "::")

COMMENT = r"(?://|#|@REM|REM|::)"
COPYRIGHT_RE = re.compile(rf"^{COMMENT}\s*SPDX-FileCopyrightText:\s*(\S.*?)\s*$")
IDENTIFIER_RE = re.compile(rf"^{COMMENT}\s*SPDX-License-Identifier:\s*(\S.*?)\s*$")
PORT_ROW_RE = re.compile(r"^\| `(crates/[^`]+)` \|.*$", re.MULTILINE)
ANTS_UPSTREAM_RE = re.compile(r"ANTs|itkants|antsApplyTransforms")

THIRD_PARTY_ROOTS = ("resources/", "crates/*/assets/")
WYSS_AUTHORED = (
    "resources/README.md",
    "resources/.gitattributes",
    "resources/atlases/ahead/README.md",
    "resources/atlases/ahead/manifest.json",
    "resources/templates/space/MNI152NLin2009bAsym/res-1mm/template.json",
    "crates/dbsloc-viz/assets/brand/*",
)
SENTINELS = (
    ("resources/ants/win/vcruntime140.dll", "LicenseRef-Microsoft-VCRedist"),
    ("resources/ants/linux/antsRegistration", "Apache-2.0"),
    ("resources/dcm2niix/win/dcm2niix.exe", "LicenseRef-dcm2niix"),
    (
        "resources/atlases/leaddbs/**",
        "GPL-3.0-only AND LicenseRef-LeadDBS-Atlas-Terms",
    ),
    ("resources/atlases/ahead/structures_mni09b/*.nii.gz", "CC-BY-4.0"),
    (
        "resources/templates/space/MNI152NLin2009bAsym/res-1mm/t1.nii.gz",
        "LicenseRef-MNI-ICBM2009",
    ),
    ("crates/dbsloc-viz/assets/fonts/IBMPlexSans-Regular.ttf", "OFL-1.1"),
    ("fastwarp/crates/fastwarp-reg/tests/data/ms0.mat", "Apache-2.0"),
    ("fastwarp/NOTICE", "Apache-2.0"),
    ("crates/dbsloc-viz/assets/brand/wyss-center-icon-256.png", GPL),
    ("crates/dbsloc-viz/assets/brand/wyss-center-icon-inverse-256.png", GPL),
    ("crates/dbsloc-viz/assets/brand/dbs-loc-icon-256.png", GPL),
    ("packaging/wix/logo-64.png", GPL),
)


def _files() -> list[str]:
    try:
        out = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=ROOT,
            capture_output=True,
            check=True,
        ).stdout
    except OSError, subprocess.CalledProcessError:
        pytest.skip("git is not available or the tree is not a git checkout")
    names = sorted({n for n in out.decode("utf-8").split("\0") if n})
    return [n for n in names if (ROOT / n).is_file()]


def _is_source(name: str) -> bool:
    return Path(name).suffix in SOURCE_SUFFIXES


def _crate_of(name: str) -> str | None:
    parts = name.split("/")
    if len(parts) > 3 and parts[0] == GPUREG and parts[1] == "crates":
        return parts[2]
    if len(parts) > 2 and parts[0] == "crates":
        return parts[1]
    return None


def _in_gpureg(name: str) -> bool:
    return name.startswith(f"{GPUREG}/")


def _expected_licence(name: str) -> str:
    return APACHE if _in_gpureg(name) else GPL


def _head(name: str) -> list[str]:
    text = (ROOT / name).read_bytes().decode("utf-8", errors="replace")
    return text.lstrip("﻿").splitlines()[:HEADER_LINES]


def _reuse_regex(pattern: str) -> re.Pattern[str]:
    out = []
    i = 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out) + r"\Z")


def _annotations() -> list[dict[str, Any]]:
    data = tomllib.loads((ROOT / "REUSE.toml").read_text(encoding="utf-8"))
    return list(data.get("annotations", []))


def _effective_annotation(
    name: str, annotations: list[dict[str, Any]]
) -> dict[str, Any] | None:
    match = None
    for ann in annotations:
        if any(_reuse_regex(p).match(name) for p in _as_list(ann.get("path"))):
            match = ann
    return match


def _is_default_annotation(ann: dict[str, Any] | None) -> bool:
    return ann is None or _as_list(ann.get("path")) == ["**"]


def _effective_licence(name: str, annotations: list[dict[str, Any]]) -> str | None:
    ann = _effective_annotation(name, annotations)
    if ann is None:
        return None
    return " AND ".join(_as_list(ann.get("SPDX-License-Identifier")))


def _is_comment(line: str) -> bool:
    return line.lstrip().startswith(COMMENT_MARKERS)


def _as_list(value: Any) -> list[str]:
    if value is None:
        return []
    return [value] if isinstance(value, str) else list(value)


def _split_expression(expression: str) -> list[str]:
    parts = re.split(r"\s+(?:AND|OR|WITH)\s+|[()]", expression)
    return [p.strip() for p in parts if p.strip()]


def _manifests() -> list[str]:
    root = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    members = list(root["workspace"]["members"])
    manifests = [f"{m}/Cargo.toml" for m in members]
    manifests.append("crates/dbsloc-web/Cargo.toml")
    gpureg = tomllib.loads((ROOT / GPUREG / "Cargo.toml").read_text("utf-8"))
    manifests.extend(f"{GPUREG}/{m}/Cargo.toml" for m in gpureg["workspace"]["members"])
    return manifests


def _dependency_tables(
    manifest: dict[str, Any],
) -> Iterator[tuple[str, dict[str, Any]]]:
    kinds = ("dependencies", "build-dependencies", "dev-dependencies")
    for kind in kinds:
        if kind in manifest:
            yield kind, manifest[kind]
    for target, tables in manifest.get("target", {}).items():
        for kind in kinds:
            if kind in tables:
                yield f"target.{target}.{kind}", tables[kind]


def test_source_files_carry_spdx_header() -> None:
    bad = []
    for name in _files():
        if not _is_source(name):
            continue
        head = _head(name)
        copyrights = [m for line in head if (m := COPYRIGHT_RE.match(line))]
        identifiers = [m.group(1) for line in head if (m := IDENTIFIER_RE.match(line))]
        want = _expected_licence(name)
        if not copyrights:
            bad.append(f"{name}: no SPDX-FileCopyrightText line")
        if len(identifiers) != 1:
            bad.append(
                f"{name}: {len(identifiers)} SPDX-License-Identifier lines (want 1)"
            )
        elif identifiers[0] != want:
            bad.append(f"{name}: licence {identifiers[0]}, expected {want}")
    assert not bad, (
        "SPDX header problems; copy the header of a neighbouring file in the same "
        "directory (docs/LICENSING.md):\n" + "\n".join(bad)
    )


def test_ported_files_are_apache_and_credit_itk() -> None:
    table = (ROOT / GPUREG / "docs" / "THIRD_PARTY_PORTS.md").read_text(
        encoding="utf-8"
    )
    rows = {f"{GPUREG}/{m.group(1)}": m.group(0) for m in PORT_ROW_RE.finditer(table)}
    assert rows, "no ported-file rows found in fastwarp/docs/THIRD_PARTY_PORTS.md"
    bad = []
    for name, row in rows.items():
        if _crate_of(name) not in GPUREG_CRATES:
            bad.append(
                f"{name}: ported file outside an Apache-2.0 crate (move it or relicense the crate)"
            )
            continue
        if not (ROOT / name).is_file():
            bad.append(
                f"{name}: listed in fastwarp/docs/THIRD_PARTY_PORTS.md but missing (fix the table)"
            )
            continue
        head = _head(name)
        if not any(APACHE_NOTICE in line for line in head):
            bad.append(f"{name}: Apache licence notice missing from the header")
        wanted = [ITK_COPYRIGHT, NUMFOCUS_COPYRIGHT, WYSS_COPYRIGHT]
        has_ants = bool(ANTS_UPSTREAM_RE.search(row))
        if has_ants:
            wanted.append(ANTS_COPYRIGHT)
        for holder in wanted:
            if not any(f"SPDX-FileCopyrightText: {holder}" in line for line in head):
                bad.append(f"{name}: missing 'SPDX-FileCopyrightText: {holder}'")
        if not has_ants and any(
            f"SPDX-FileCopyrightText: {ANTS_COPYRIGHT}" in line for line in head
        ):
            bad.append(
                f"{name}: credits '{ANTS_COPYRIGHT}' but its fastwarp/docs/THIRD_PARTY_PORTS.md row names no ANTs upstream"
            )
    for name in _files():
        if name in rows or not _is_source(name):
            continue
        if any(APACHE_NOTICE in line and _is_comment(line) for line in _head(name)):
            bad.append(
                f"{name}: carries the ported-file Apache notice but has no row in fastwarp/docs/THIRD_PARTY_PORTS.md"
            )
    assert not bad, (
        "ported files must use the header template in fastwarp/docs/THIRD_PARTY_PORTS.md:\n"
        + "\n".join(bad)
    )


def test_reuse_toml_covers_non_source_files() -> None:
    files = _files()
    annotations = _annotations()
    problems = []
    compiled = []
    for index, ann in enumerate(annotations):
        label = f"annotation #{index + 1} ({', '.join(_as_list(ann.get('path')))})"
        if not _as_list(ann.get("SPDX-FileCopyrightText")):
            problems.append(f"{label}: no SPDX-FileCopyrightText")
        if not _as_list(ann.get("SPDX-License-Identifier")):
            problems.append(f"{label}: no SPDX-License-Identifier")
        for pattern in _as_list(ann.get("path")):
            compiled.append((label, pattern, _reuse_regex(pattern)))
    for label, pattern, regex in compiled:
        if not any(regex.match(name) for name in files):
            problems.append(
                f"{label}: pattern '{pattern}' matches no file (stale path)"
            )
    for name in files:
        base = name.rsplit("/", 1)[-1]
        if _is_source(name) or name.startswith(("LICENSES/", f"{GPUREG}/LICENSES/")):
            continue
        if base.startswith(("LICENSE", "COPYING")):
            continue
        if not any(regex.match(name) for _, _, regex in compiled):
            problems.append(f"{name}: not covered by any REUSE.toml annotation")
    assert not problems, "REUSE.toml problems (edit REUSE.toml):\n" + "\n".join(
        problems
    )


def test_third_party_files_are_not_left_on_the_default_licence() -> None:
    files = _files()
    annotations = _annotations()
    allow = [(pattern, _reuse_regex(pattern)) for pattern in WYSS_AUTHORED]
    stale = [p for p, regex in allow if not any(regex.match(n) for n in files)]
    assert not stale, (
        "Wyss-authored allow-list entries in tests/test_licensing.py match no file "
        "(stale; remove them): " + ", ".join(stale)
    )
    roots = [_reuse_regex(root + "**") for root in THIRD_PARTY_ROOTS]
    unlabelled = []
    for name in files:
        base = name.rsplit("/", 1)[-1]
        if _is_source(name) or base.startswith(("LICENSE", "COPYING")):
            continue
        if not any(regex.match(name) for regex in roots):
            continue
        if any(regex.match(name) for _, regex in allow):
            continue
        if _is_default_annotation(_effective_annotation(name, annotations)):
            unlabelled.append(name)
    assert not unlabelled, (
        "files under resources/ or crates/*/assets/ fall back to the GPL-3.0-only "
        "default: add an annotation to REUSE.toml with the third-party licence, or "
        "add the path to the Wyss-authored allow-list if Wyss made it:\n"
        + "\n".join(unlabelled)
    )


@pytest.mark.parametrize(("pattern", "licence"), SENTINELS)
def test_sentinel_effective_licences(pattern: str, licence: str) -> None:
    regex = _reuse_regex(pattern)
    matches = [n for n in _files() if regex.match(n)]
    assert matches, f"sentinel pattern '{pattern}' matches no file; update SENTINELS"
    annotations = _annotations()
    wrong = [
        f"{n}: {_effective_licence(n, annotations)}"
        for n in matches
        if _effective_licence(n, annotations) != licence
    ]
    assert not wrong, (
        f"effective REUSE.toml licence should be '{licence}' "
        "(last matching annotation wins; fix REUSE.toml):\n" + "\n".join(wrong[:10])
    )


def test_licence_texts_match_used_identifiers() -> None:
    used: set[str] = set()
    for name in _files():
        if _is_source(name):
            for line in _head(name):
                if m := IDENTIFIER_RE.match(line):
                    used.update(_split_expression(m.group(1)))
    for ann in _annotations():
        for expression in _as_list(ann.get("SPDX-License-Identifier")):
            used.update(_split_expression(expression))
    available = {p.stem for p in (ROOT / "LICENSES").glob("*.txt")}
    missing = sorted(used - available)
    unused = sorted(available - used)
    assert not missing, (
        "licence identifiers without a text file; add LICENSES/<id>.txt: "
        + ", ".join(missing)
    )
    assert not unused, (
        "files in LICENSES/ that no header or REUSE.toml annotation uses; delete them: "
        + ", ".join(unused)
    )


def test_cargo_licence_fields_and_dependency_direction() -> None:
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    assert workspace["workspace"]["members"], "root workspace has no members"
    gpureg = tomllib.loads((ROOT / GPUREG / "Cargo.toml").read_text("utf-8"))
    shared = gpureg["workspace"].get("dependencies", {})
    inherited = gpureg["workspace"].get("package", {}).get("license")
    problems = []
    for rel in _manifests():
        manifest = tomllib.loads((ROOT / rel).read_text(encoding="utf-8"))
        crate = Path(rel).parent.name
        want = APACHE if rel.startswith(f"{GPUREG}/") else GPL
        if (crate in GPUREG_CRATES) != (want == APACHE):
            problems.append(
                f"{rel}: crate name {crate} and location disagree on licence"
            )
        have = manifest["package"].get("license")
        if have == {"workspace": True} and rel.startswith(f"{GPUREG}/"):
            have = inherited
        if have != want:
            problems.append(f"{rel}: license = {have!r}, expected {want!r}")
        if want != APACHE:
            continue
        for kind, table in _dependency_tables(manifest):
            for dep, spec in table.items():
                if not isinstance(spec, dict):
                    continue
                if spec.get("workspace") is True:
                    spec = shared.get(dep, {})
                    if not isinstance(spec, dict):
                        continue
                path = spec.get("path")
                if path is None:
                    continue
                target = (ROOT / rel).parent.joinpath(path).resolve().name
                if target not in GPUREG_CRATES:
                    problems.append(
                        f"{rel}: [{kind}] {dep} -> {target} is not an Apache-2.0 crate "
                        "(an Apache-2.0 crate may depend only on Apache-2.0 crates, never on a GPL-3.0-only one; move the code or the crate)"
                    )
    assert not problems, "Cargo licence problems:\n" + "\n".join(problems)


def _licence_text(path: Path) -> str:
    text = path.read_text(encoding="utf-8")
    text = text.replace("http://www.apache.org/", "https://www.apache.org/")
    return " ".join(text.split())


def test_apache_crates_ship_licence_and_notice() -> None:
    text = _licence_text(ROOT / "LICENSES" / "Apache-2.0.txt")
    problems = []
    for rel in (f"{GPUREG}/LICENSE", f"{GPUREG}/LICENSES/Apache-2.0.txt"):
        licence = ROOT / rel
        if not licence.is_file():
            problems.append(f"{rel} missing")
        elif _licence_text(licence) != text:
            problems.append(f"{rel} differs from LICENSES/Apache-2.0.txt")
    for crate in sorted(APACHE_CRATES):
        licence = ROOT / GPUREG / "crates" / crate / "LICENSE"
        if not licence.is_file():
            problems.append(f"{GPUREG}/crates/{crate}/LICENSE missing")
        elif _licence_text(licence) != text:
            problems.append(
                f"{GPUREG}/crates/{crate}/LICENSE differs from LICENSES/Apache-2.0.txt"
            )
    for crate in ("fastwarp-reg", "fastwarp-gpu"):
        if not (ROOT / GPUREG / "crates" / crate / "NOTICE").is_file():
            problems.append(f"{GPUREG}/crates/{crate}/NOTICE missing")
    assert not problems, (
        "Apache-2.0 crate files; copy LICENSES/Apache-2.0.txt to the crate:\n"
        + "\n".join(problems)
    )
