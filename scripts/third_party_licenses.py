# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Collect the licence texts of the Rust libraries linked into the shipped programs.

    uv run python scripts/third_party_licenses.py --out THIRD_PARTY_LICENSES.md [--target TRIPLE] [--offline]

Walks the dependency graph reported by ``cargo metadata`` from the packages
``dbsloc-app`` (``dbs-localizer``) and ``dbsloc-cli`` (``dbsloc``), follows normal
dependencies only, and writes one document with the licence expression and the
licence files of every library. The workspace crates under ``crates/`` (GPL-3.0-only,
the licence of this repository) are left out; the Apache-2.0 engine crates under
``fastwarp/`` are listed in a section of their own.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ENGINE_DIR = "fastwarp"
ROOT_PACKAGES = ("dbsloc-app", "dbsloc-cli")
PROGRAMS = ("dbs-localizer", "dbsloc")
LICENCE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")
DEEP_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "OFL", "UFL")
LICENCE_DIRS = frozenset({"licenses", "license", "licences", "licence"})
FONT_SUFFIXES = (".ttf", ".otf", ".woff", ".woff2")
DEEP_DEPTH = 3
MAX_FILE_BYTES = 1_000_000
NO_LICENCE_FILE_ALLOWED: frozenset[str] = frozenset(
    {
        "accesskit",
        "accesskit_atspi_common",
        "accesskit_consumer",
        "accesskit_macos",
        "accesskit_unix",
        "accesskit_windows",
        "accesskit_winit",
        "alloc-stdlib",
        "block2",
        "clipboard-win",
        "dispatch",
        "dispatch2",
        "ecolor",
        "eframe",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "egui_glow",
        "emath",
        "epaint",
        "objc-sys",
        "objc2",
        "objc2-app-kit",
        "objc2-core-data",
        "objc2-core-foundation",
        "objc2-core-graphics",
        "objc2-core-image",
        "objc2-encode",
        "objc2-io-surface",
        "objc2-metal",
        "objc2-quartz-core",
        "profiling",
        "spirv",
    }
)
STANDARD_IDS = (
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "BSL-1.0",
)
FILE_ONLY = "(licence file only)"


class LicenceError(Exception):
    pass


@dataclass
class Package:
    name: str
    version: str
    license: str | None
    repository: str | None
    engine: bool
    files: list[tuple[str, str]] = field(default_factory=list)

    @property
    def expression(self) -> str:
        return self.license or FILE_ONLY

    @property
    def label(self) -> str:
        return f"{self.name} {self.version}"


@dataclass
class Bundle:
    target: str
    packages: list[Package]

    @property
    def third_party(self) -> list[Package]:
        return [p for p in self.packages if not p.engine]

    @property
    def engine(self) -> list[Package]:
        return [p for p in self.packages if p.engine]


def host_triple() -> str:
    p = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=False)
    if p.returncode != 0:
        raise LicenceError(f"rustc -vV failed:\n{p.stderr}")
    for line in p.stdout.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    raise LicenceError("rustc -vV did not report a host triple")


def cargo_metadata(root: Path, target: str, offline: bool) -> dict:
    cmd = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--locked",
        "--filter-platform",
        target,
    ]
    if offline:
        cmd.append("--offline")
    p = subprocess.run(
        cmd, cwd=root, capture_output=True, text=True, encoding="utf-8", check=False
    )
    if p.returncode != 0:
        raise LicenceError(f"cargo metadata failed:\n{p.stderr}")
    return json.loads(p.stdout)


def linked_ids(metadata: dict) -> set[str]:
    names = {p["id"]: p["name"] for p in metadata["packages"]}
    roots = [
        pid for pid in metadata["workspace_members"] if names.get(pid) in ROOT_PACKAGES
    ]
    if not roots:
        raise LicenceError(f"workspace packages {', '.join(ROOT_PACKAGES)} not found")
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    seen: set[str] = set()
    queue = list(roots)
    while queue:
        pid = queue.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes.get(pid, {}).get("deps", []):
            if any(k.get("kind") is None for k in dep.get("dep_kinds", [])):
                queue.append(dep["pkg"])
    return seen


def is_under(path: Path, parent: Path) -> bool:
    try:
        path.resolve().relative_to(parent.resolve())
    except ValueError:
        return False
    return True


def read_text(path: Path, label: str | None = None) -> str:
    with path.open("rb") as fh:
        data = fh.read(MAX_FILE_BYTES + 1)
    text = data[:MAX_FILE_BYTES].decode("utf-8", errors="replace")
    if len(data) > MAX_FILE_BYTES:
        text += (
            f"\n\n[Truncated: {label or path.name} is longer than {MAX_FILE_BYTES} "
            f"bytes; only the first {MAX_FILE_BYTES} bytes are shown.]\n"
        )
    return text


def relative(directory: Path, path: Path) -> str:
    return path.relative_to(directory).as_posix()


def is_deep_candidate(path: Path) -> bool:
    if path.name.upper().startswith(DEEP_PREFIXES):
        return True
    if path.suffix.lower() == ".txt":
        upper = path.name.upper()
        if "LICENSE" in upper or "LICENCE" in upper:
            return True
        return any(path.with_suffix(s).is_file() for s in FONT_SUFFIXES)
    return False


def deep_files(directory: Path, found: dict[str, str]) -> None:
    def walk(current: Path, depth: int) -> None:
        for p in sorted(current.iterdir()):
            if p.is_dir() and depth < DEEP_DEPTH and not p.is_symlink():
                walk(p, depth + 1)
            elif p.is_file() and is_deep_candidate(p):
                name = relative(directory, p)
                if name not in found:
                    found[name] = read_text(p, name)

    walk(directory, 1)


def licence_files(directory: Path, extra: str | None = None) -> list[tuple[str, str]]:
    found: dict[str, str] = {}
    if directory.is_dir():
        for p in sorted(directory.iterdir()):
            if p.is_file() and p.name.upper().startswith(LICENCE_PREFIXES):
                found[p.name] = read_text(p)
            elif p.is_dir() and p.name.lower() in LICENCE_DIRS:
                for q in sorted(p.iterdir()):
                    if q.is_file():
                        name = relative(directory, q)
                        found[name] = read_text(q, name)
        if extra and extra not in found and (directory / extra).is_file():
            found[extra] = read_text(directory / extra)
        if not any(t.strip() for t in found.values()):
            deep_files(directory, found)
    return sorted((n, t) for n, t in found.items() if t.strip())


def collect(metadata: dict, root: Path, target: str) -> Bundle:
    ids = linked_ids(metadata)
    engine_root = root / ENGINE_DIR
    packages: list[Package] = []
    missing: list[str] = []
    for pkg in metadata["packages"]:
        if pkg["id"] not in ids:
            continue
        manifest_dir = Path(pkg["manifest_path"]).parent
        engine = False
        if pkg.get("source") is None:
            if not is_under(manifest_dir, engine_root):
                continue
            engine = True
        files = licence_files(manifest_dir, pkg.get("license_file"))
        if engine and not files:
            files = licence_files(engine_root)
        if not pkg.get("license") and not files:
            raise LicenceError(
                f"{pkg['name']} {pkg['version']} has neither a licence expression "
                "nor a licence file"
            )
        if not files and pkg["name"] not in NO_LICENCE_FILE_ALLOWED:
            missing.append(f"{pkg['name']} {pkg['version']}")
        packages.append(
            Package(
                pkg["name"],
                pkg["version"],
                pkg.get("license"),
                pkg.get("repository"),
                engine,
                files,
            )
        )
    if missing:
        raise LicenceError(
            "no licence file found in the published package of: "
            + ", ".join(missing)
            + ". Check the crate's repository for its licence text and, if it is "
            "only there, add the crate name to NO_LICENCE_FILE_ALLOWED in "
            "scripts/third_party_licenses.py"
        )
    packages.sort(key=lambda p: (p.name, p.version))
    return Bundle(target, packages)


def normalise(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\r", "\n").strip("\n")


def fenced(text: str) -> list[str]:
    body = normalise(text)
    fence = "````"
    while fence in body:
        fence += "`"
    return [fence + "text", body, fence, ""]


def standard_text_donors(packages: list[Package]) -> dict[str, Package]:
    donors: dict[str, Package] = {}
    for ident in STANDARD_IDS:
        for p in packages:
            if p.license == ident and len(p.files) == 1:
                donors[ident] = p
                break
    return donors


def package_section(
    p: Package, shared_id: dict[str, int], donors: dict[str, Package]
) -> list[str]:
    out = [f"### {p.name} {p.version}", "", f"- Licence: {p.expression}"]
    if p.repository:
        out.append(f"- Repository: {p.repository}")
    out.append("")
    if not p.files:
        where = f" ({p.repository})" if p.repository else ""
        out.append(
            "The published package contains no licence file; the licence text is in "
            f"the package's source repository{where}."
        )
        donor = donors.get(p.license or "")
        if donor is not None and donor is not p:
            out.append(
                f"The standard text of {p.license} is reproduced in this document "
                f"under {donor.label}."
            )
        out.append("")
    for name, text in p.files:
        key = normalise(text)
        if key in shared_id:
            out += [f"`{name}`: see shared text {shared_id[key]} below.", ""]
        else:
            out += [f"`{name}`:", ""] + fenced(key)
    return out


def render(bundle: Bundle) -> str:
    third = bundle.third_party
    engine = bundle.engine
    no_file = [p for p in bundle.packages if not p.files]
    donors = standard_text_donors(bundle.packages)
    users: dict[str, list[str]] = {}
    for p in bundle.packages:
        for _name, text in p.files:
            who = users.setdefault(normalise(text), [])
            if p.label not in who:
                who.append(p.label)
    shared = sorted(
        (t for t, who in users.items() if len(who) > 1), key=lambda t: users[t]
    )
    shared_id = {t: i for i, t in enumerate(shared, 1)}
    out = [
        "# Third-party licences of the Rust libraries",
        "",
        (
            f"Wyss DBS Localizer ships two programs, `{PROGRAMS[0]}` (graphical application) "
            f"and `{PROGRAMS[1]}` (command line). They link the Rust libraries listed below. "
            "Each library is distributed under the licence named with it; its copyright "
            "notice and licence text are reproduced here as those licences require."
        ),
        "",
        f"- Target: `{bundle.target}`",
        f"- Third-party libraries: {len(third)}",
        f"- Libraries of the Apache-2.0 registration engine (`{ENGINE_DIR}/`): {len(engine)}",
        f"- Packages without a licence file in the published package: {len(no_file)}"
        + (f" ({', '.join(p.label for p in no_file)})" if no_file else ""),
        "",
        "| Licence expression | Packages |",
        "| --- | ---: |",
    ]
    counts = Counter(p.expression for p in bundle.packages)
    for expr, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])):
        out.append(f"| {expr.replace('|', '/')} | {n} |")
    out.append("")
    if third:
        out += ["## Libraries", ""]
        for p in third:
            out += package_section(p, shared_id, donors)
    if engine:
        out += [
            "## Registration engine (Apache-2.0)",
            "",
            (
                f"The crates under `{ENGINE_DIR}/` are part of this project and are licensed "
                "under the Apache License, Version 2.0 (`LICENSES/Apache-2.0.txt`)."
            ),
            "",
        ]
        for p in engine:
            out += package_section(p, shared_id, donors)
    if shared_id:
        out += ["## Shared licence texts", ""]
        for text, i in shared_id.items():
            out += [f"### Text {i}", "", "Used by: " + ", ".join(users[text]), ""]
            out += fenced(text)
    return "\n".join(out).rstrip("\n") + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--out", type=Path, required=True, help="output file")
    ap.add_argument("--target", help="target triple (default: the host triple)")
    ap.add_argument("--offline", action="store_true", help="pass --offline to cargo")
    args = ap.parse_args(argv)
    try:
        target = args.target or host_triple()
        bundle = collect(cargo_metadata(REPO, target, args.offline), REPO, target)
    except LicenceError as e:
        print(f"third_party_licenses: error: {e}", file=sys.stderr)
        return 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(render(bundle), encoding="utf-8", newline="\n")
    print(
        f"third_party_licenses: {len(bundle.packages)} packages, "
        f"{sum(not p.files for p in bundle.packages)} without a licence file, "
        f"wrote {args.out}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
