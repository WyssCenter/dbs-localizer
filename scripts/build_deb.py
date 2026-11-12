# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Build (and optionally install-test) the Debian / Ubuntu package of Wyss DBS Localizer.

    uv run python scripts/build_deb.py [--stage DIR] [--out DIR] [--version X.Y.Z] [--test]

The stage is the assembled Linux archive folder produced by ``scripts/package_release.py``
(default ``dist/dbsloc-linux``). The package installs it unchanged under
``/opt/dbs-localizer`` (the executables find ``resources/`` by walking up from their own
path), links the five executables into ``/usr/bin`` and adds the menu entry and icon.
``Depends`` is derived from the ELF ``NEEDED`` entries of the shipped executables; a library
without a known Debian package is an error, so a new dependency is never missed silently.
``--test`` installs the built package with apt (needs sudo), checks it and removes it again.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import sys
import tempfile
import tomllib
from pathlib import Path

try:
    from scripts import package_release as pr
except ImportError:
    import package_release as pr

REPO = Path(__file__).resolve().parents[1]
PACKAGE = "dbs-localizer"
REPO_URL = pr.REPO_URL
COPYRIGHT_HOLDER = "Wyss Center for Bio and Neuroengineering"
PREFIX = "opt/dbs-localizer"
MAINTAINER = "Richard Köhler <richard.koehler@wysscenter.ch>"
ARCH = "amd64"
SHORT_DESCRIPTION = "Wyss DBS Localizer, deep brain stimulation electrode localization"
LONG_DESCRIPTION = (
    "Localizes deep brain stimulation electrodes in MRI and CT images: image import,\n"
    "registration to a template, lead reconstruction, orientation, stimulation volumes\n"
    "and group analyses. Includes the graphical application, the dbsloc command line\n"
    "tool, the 3D viewer, and bundled ANTs, dcm2niix and a MNI template."
)
DESKTOP_TEMPLATE = REPO / "packaging" / "linux" / "dbs-localizer.desktop"
ICON_SRC = pr.ICON_SRC
EXECUTABLES = (pr.GUI, pr.CLI)

GTK_ALT = "libgtk-3-0t64 | libgtk-3-0"
GLIB_ALT = "libglib2.0-0t64 | libglib2.0-0"
ATK_ALT = "libatk1.0-0t64 | libatk1.0-0"
SONAME_PACKAGES: dict[str, str] = {
    "libc.so.6": "libc6",
    "libm.so.6": "libc6",
    "libdl.so.2": "libc6",
    "libpthread.so.0": "libc6",
    "librt.so.1": "libc6",
    "libutil.so.1": "libc6",
    "libresolv.so.2": "libc6",
    "libgcc_s.so.1": "libgcc-s1",
    "libstdc++.so.6": "libstdc++6",
    "libz.so.1": "zlib1g",
    "libgtk-3.so.0": GTK_ALT,
    "libgdk-3.so.0": GTK_ALT,
    "libglib-2.0.so.0": GLIB_ALT,
    "libgobject-2.0.so.0": GLIB_ALT,
    "libgio-2.0.so.0": GLIB_ALT,
    "libgmodule-2.0.so.0": GLIB_ALT,
    "libatk-1.0.so.0": ATK_ALT,
    "libcairo.so.2": "libcairo2",
    "libcairo-gobject.so.2": "libcairo-gobject2",
    "libpango-1.0.so.0": "libpango-1.0-0",
    "libpangocairo-1.0.so.0": "libpangocairo-1.0-0",
    "libgdk_pixbuf-2.0.so.0": "libgdk-pixbuf-2.0-0",
    "libfontconfig.so.1": "libfontconfig1",
    "libfreetype.so.6": "libfreetype6",
    "libvulkan.so.1": "libvulkan1",
    "libxkbcommon.so.0": "libxkbcommon0",
    "libxkbcommon-x11.so.0": "libxkbcommon-x11-0",
    "libX11.so.6": "libx11-6",
    "libxcb.so.1": "libxcb1",
    "libwayland-client.so.0": "libwayland-client0",
}
RUNTIME_LOADED = (GTK_ALT, "libvulkan1", "libxkbcommon0", "libfontconfig1")
RECOMMENDS = ("mesa-vulkan-drivers",)


class DebError(Exception):
    pass


def deb_version(version: str) -> str:
    """Debian upstream version: ``1.0.0-rc.1`` -> ``1.0.0~rc.1``, ``+build`` kept."""
    core, plus, build = version.partition("+")
    head, dash, pre = core.partition("-")
    if dash:
        core = f"{head}~{pre.replace('-', '.')}"
    out = core + plus + build
    allowed = set("0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.+~")
    if not out or not out[0].isdigit() or any(c not in allowed for c in out):
        raise DebError(f"cannot turn {version!r} into a Debian version")
    return out


def package_for_soname(soname: str) -> str:
    if soname.startswith("ld-linux"):
        return "libc6"
    try:
        return SONAME_PACKAGES[soname]
    except KeyError:
        raise DebError(
            f"no Debian package known for {soname}: add it to SONAME_PACKAGES in scripts/build_deb.py"
        ) from None


def parse_needed(readelf_dynamic: str) -> list[str]:
    names = []
    for line in readelf_dynamic.splitlines():
        if "(NEEDED)" in line and "[" in line:
            names.append(line.split("[", 1)[1].split("]", 1)[0])
    return names


def is_elf(path: Path) -> bool:
    with path.open("rb") as f:
        return f.read(4) == b"\x7fELF"


def elf_needed(path: Path) -> list[str]:
    p = pr.run(["readelf", "-d", str(path)], timeout=60)
    if p.returncode != 0:
        raise DebError(f"readelf -d {path.name} failed: {p.stderr.strip()}")
    return parse_needed(p.stdout)


def depends_for(bin_dir: Path) -> list[str]:
    needed: set[str] = set()
    for exe in sorted(bin_dir.iterdir()):
        if exe.is_file() and is_elf(exe):
            needed.update(elf_needed(exe))
    packages = {package_for_soname(n) for n in needed}
    packages.update(RUNTIME_LOADED)
    packages.discard("libc6")
    return sorted(packages)


def control_text(
    version: str,
    installed_size_kib: int,
    glibc: str,
    packages: tuple[str, ...] | list[str] = RUNTIME_LOADED,
) -> str:
    libc = f"libc6 (>= {glibc})" if glibc and glibc != "unknown" else "libc6"
    depends = ", ".join([libc, *(p for p in packages if p != "libc6")])
    long_lines = "\n".join(
        f" {ln}" if ln else " ." for ln in LONG_DESCRIPTION.splitlines()
    )
    return (
        f"Package: {PACKAGE}\n"
        f"Version: {version}\n"
        "Section: science\n"
        "Priority: optional\n"
        f"Architecture: {ARCH}\n"
        f"Maintainer: {MAINTAINER}\n"
        f"Installed-Size: {installed_size_kib}\n"
        f"Homepage: {REPO_URL}\n"
        f"Depends: {depends}\n"
        f"Recommends: {', '.join(RECOMMENDS)}\n"
        f"Description: {SHORT_DESCRIPTION}\n"
        f"{long_lines}\n"
    )


def copyright_text(version: str) -> str:
    return (
        "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n"
        "Upstream-Name: Wyss DBS Localizer\n"
        "Upstream-Contact: " + MAINTAINER + "\n"
        f"Source: {REPO_URL}\n"
        f" The source code of this package is the tag {pr.source_tag(version)}:\n"
        f" {pr.source_url(version)}\n"
        "\n"
        "Files: *\n"
        f"Copyright: 2026 {COPYRIGHT_HOLDER}\n"
        "License: GPL-3.0-only\n"
        "Comment: The programs are under this licence. The package also contains\n"
        " third-party components under their own licences (registration engine,\n"
        " ANTs, dcm2niix, templates, atlases, fonts, Rust libraries); they are\n"
        f" listed in /{PREFIX}/NOTICE and /{PREFIX}/{pr.THIRD_PARTY_NAME}.\n"
        "\n"
        "License: GPL-3.0-only\n"
        " This program is free software: you can redistribute it and/or modify it\n"
        " under the terms of the GNU General Public License version 3 as published\n"
        " by the Free Software Foundation.\n"
        " .\n"
        " This program is distributed in the hope that it will be useful, but\n"
        " WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY\n"
        " or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License\n"
        " for more details.\n"
        " .\n"
        f" The full text of the licence is in /{PREFIX}/LICENSE and, on Debian\n"
        " systems, in /usr/share/common-licenses/GPL-3. Licences of bundled\n"
        f" third-party components are in /{PREFIX}/NOTICE, /{PREFIX}/LICENSES/\n"
        f" and /{PREFIX}/{pr.THIRD_PARTY_NAME}.\n"
    )


def desktop_entry(exec_path: str) -> str:
    quoted = f'"{exec_path}"' if " " in exec_path else exec_path
    return DESKTOP_TEMPLATE.read_text(encoding="utf-8").replace("@EXEC@", quoted)


def workspace_version() -> str:
    data = tomllib.loads((REPO / "Cargo.toml").read_text(encoding="utf-8"))
    return str(data["workspace"]["package"]["version"])


def wants_exec(path: Path) -> bool:
    if path.stat().st_mode & 0o111:
        return True
    return ".so" not in path.name and is_elf(path)


def copy_tree(src: Path, dst: Path) -> None:
    for p in sorted(src.rglob("*")):
        out = dst / p.relative_to(src)
        if p.is_dir():
            out.mkdir(parents=True, exist_ok=True)
            out.chmod(0o755)
        elif p.is_file():
            out.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(p, out)
            out.chmod(0o755 if wants_exec(p) else 0o644)


def write_file(path: Path, text: str | bytes, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(text, str):
        path.write_text(text, encoding="utf-8")
    else:
        path.write_bytes(text)
    path.chmod(mode)


def chmod_dirs(root: Path) -> None:
    root.chmod(0o755)
    for d in root.rglob("*"):
        if d.is_dir() and not d.is_symlink():
            d.chmod(0o755)


def installed_size_kib(root: Path) -> int:
    total = sum(
        p.stat().st_size for p in root.rglob("*") if p.is_file() and not p.is_symlink()
    )
    return math.ceil(total / 1024)


def build_tree(stage: Path, tree: Path, version: str, upstream_version: str) -> int:
    bin_dir = stage / "bin"
    if not bin_dir.is_dir() or not (stage / "resources").is_dir():
        raise DebError(
            f"{stage} is not an assembled Linux archive folder (bin/, resources/)"
        )
    missing = [e for e in EXECUTABLES if not (bin_dir / e).is_file()]
    if missing:
        raise DebError(f"missing from {bin_dir}: {', '.join(missing)}")
    glibc = pr.glibc_requirement(bin_dir / pr.GUI)
    packages = depends_for(bin_dir)
    opt = tree / PREFIX
    copy_tree(bin_dir, opt / "bin")
    copy_tree(stage / "resources", opt / "resources")
    for name in ("LICENSE", "NOTICE", pr.THIRD_PARTY_NAME):
        write_file(opt / name, (stage / name).read_bytes())
    copy_tree(stage / "LICENSES", opt / "LICENSES")
    usr_bin = tree / "usr" / "bin"
    usr_bin.mkdir(parents=True)
    for exe in EXECUTABLES:
        os.symlink(f"/{PREFIX}/bin/{exe}", usr_bin / exe)
    write_file(
        tree / "usr/share/applications/dbs-localizer.desktop",
        desktop_entry(f"/usr/bin/{pr.GUI}"),
    )
    write_file(
        tree / "usr/share/icons/hicolor/256x256/apps/dbs-localizer.png",
        ICON_SRC.read_bytes(),
    )
    write_file(
        tree / f"usr/share/doc/{PACKAGE}/copyright", copyright_text(upstream_version)
    )
    chmod_dirs(tree)
    size = installed_size_kib(tree)
    write_file(
        tree / "DEBIAN" / "control", control_text(version, size, glibc, packages)
    )
    (tree / "DEBIAN").chmod(0o755)
    return size


def build_deb(stage: Path, out: Path, version: str) -> Path:
    deb_ver = deb_version(version)
    out.mkdir(parents=True, exist_ok=True)
    deb = out / f"{PACKAGE}_{deb_ver}_{ARCH}.deb"
    if deb.exists():
        deb.unlink()
    with tempfile.TemporaryDirectory(prefix="dbsloc_deb_") as td:
        tree = Path(td) / PACKAGE
        tree.mkdir()
        build_tree(stage, tree, deb_ver, version)
        p = pr.run(
            ["dpkg-deb", "--build", "--root-owner-group", str(tree), str(deb)],
            timeout=1800,
        )
        if p.returncode != 0:
            raise DebError(f"dpkg-deb failed:\n{p.stdout}{p.stderr}")
    pr.log(f"wrote {deb} ({deb.stat().st_size / 1e6:.1f} MB)")
    pr.write_checksum(deb)
    return deb


def install_test(deb: Path) -> list[pr.Check]:
    checks: list[pr.Check] = []
    opt = Path("/") / PREFIX
    sudo = ["sudo"] if os.geteuid() != 0 else []

    def add(name: str, ok: bool, detail: str = "") -> None:
        checks.append(pr.Check(name, ok, detail))

    p = pr.run([*sudo, "apt-get", "install", "-y", str(deb.resolve())], timeout=1800)
    add("apt-get install", p.returncode == 0, (p.stdout + p.stderr).strip()[-300:])
    try:
        if p.returncode == 0:
            v = pr.run(["dbsloc", "--version"], env=pr.sanitized_env(), timeout=120)
            add("dbsloc --version", v.returncode == 0, v.stdout.strip())
            t = pr.run(
                ["dbsloc", "template", "list", "--json"],
                env=pr.sanitized_env(),
                timeout=120,
            )
            detail = t.stderr.strip()
            ok = t.returncode == 0
            if ok:
                rows = [r for r in json.loads(t.stdout) if r["status"] == "bundled"]
                ok = bool(rows) and all(
                    str(r["path"]).startswith(str(opt)) for r in rows
                )
                detail = rows[0]["path"] if rows else "no bundled template"
            add("template under /opt/dbs-localizer", ok, detail)
            desktop = Path("/usr/share/applications/dbs-localizer.desktop")
            add("menu entry installed", desktop.is_file(), str(desktop))
            validator = shutil.which("desktop-file-validate")
            if validator and desktop.is_file():
                d = pr.run([validator, str(desktop)])
                add(
                    "desktop-file-validate",
                    d.returncode == 0,
                    (d.stdout + d.stderr).strip(),
                )
            ldd = pr.run(["ldd", str(opt / "bin" / pr.GUI)])
            add(
                "ldd dbs-localizer",
                ldd.returncode == 0 and "not found" not in ldd.stdout,
                ", ".join(
                    ln.split()[0] for ln in ldd.stdout.splitlines() if "not found" in ln
                )
                or "all libraries resolved",
            )
    finally:
        r = pr.run([*sudo, "apt-get", "remove", "-y", PACKAGE], timeout=600)
        gone = not opt.exists() and not os.path.lexists("/usr/bin/dbsloc")
        add(
            "apt-get remove", r.returncode == 0 and gone, "" if gone else "files remain"
        )
    return checks


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--stage", type=Path, default=REPO / "dist" / "dbsloc-linux")
    ap.add_argument("--out", type=Path, default=REPO / "dist")
    ap.add_argument(
        "--version", default=None, help="default: Cargo.toml workspace version"
    )
    ap.add_argument(
        "--test", action="store_true", help="install, check and remove the package"
    )
    args = ap.parse_args(argv)
    if not sys.platform.startswith("linux"):
        raise DebError("building a .deb needs Linux")
    if shutil.which("dpkg-deb") is None:
        raise DebError(
            "dpkg-deb not found (install dpkg-dev or run on Debian / Ubuntu)"
        )
    deb = build_deb(
        args.stage.resolve(), args.out.resolve(), args.version or workspace_version()
    )
    if not args.test:
        return 0
    checks = install_test(deb)
    pr.print_table(checks, deb)
    return 0 if all(c.ok for c in checks) else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (DebError, pr.PackageError) as e:
        print(f"[deb] error: {e}", file=sys.stderr)
        sys.exit(2)
