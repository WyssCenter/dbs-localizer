# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Build, assemble and test a redistributable Wyss DBS Localizer archive.

    uv run python scripts/package_release.py [--skip-build] [--no-gui-smoke] [--e2e] [--out DIR]

Windows: ``dbsloc-win.zip`` (executables at the top level, ``resources/`` beside them).
Linux / macOS: ``dbsloc-<platform>.tar.gz`` (``bin/`` + ``resources/``, the layout
``scripts/install.sh`` expects). The archive is then extracted into a fresh temporary
directory and exercised with a sanitized environment (no ``DBSLOC_*`` variables, PATH
reduced to the system directories) so nothing can be resolved from the repository.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

try:
    from scripts import third_party_licenses as tpl
except ImportError:
    import third_party_licenses as tpl

REPO = Path(__file__).resolve().parent.parent
CLI = "dbsloc"
GUI = "dbs-localizer"
ANTS = ("antsRegistration", "antsApplyTransforms", "N4BiasFieldCorrection")
VC_REDIST_PREFIXES = ("msvcp140", "vcruntime140", "concrt140", "vccorlib140")
SYSTEM_DLL_PREFIXES = ("api-ms-win-", "ext-ms-")
RESOURCE_DIRS = ("templates", "atlases")
REPO_URL = "https://github.com/WyssCenter/dbs-localizer"
README_NAME = "README.txt"
THIRD_PARTY_NAME = "THIRD_PARTY_LICENSES.md"
INSTALL_SCRIPT = "install.sh"
ICON_SRC = REPO / "crates" / "dbsloc-viz" / "assets" / "brand" / "dbs-loc-icon-256.png"
ICON_NAME = "dbs-localizer.png"
DESKTOP_TEMPLATE_SRC = REPO / "packaging" / "linux" / "dbs-localizer.desktop"
DESKTOP_TEMPLATE_NAME = "dbs-localizer.desktop.in"


class PackageError(Exception):
    pass


def workspace_version() -> str:
    data = tomllib.loads((REPO / "Cargo.toml").read_text(encoding="utf-8"))
    return str(data["workspace"]["package"]["version"])


def source_tag(version: str) -> str:
    return f"v{version}"


def source_url(version: str) -> str:
    return f"{REPO_URL}/tree/{source_tag(version)}"


@dataclass
class Check:
    name: str
    ok: bool
    detail: str = ""


@dataclass
class Layout:
    platform: str
    root: Path
    bin_dir: Path
    resources: Path
    archive: Path
    exes: list[Path] = field(default_factory=list)

    def exe(self, name: str) -> Path:
        return self.bin_dir / exe_name(name)


def host_platform() -> str:
    return {"win32": "win", "darwin": "mac"}.get(sys.platform, "linux")


def exe_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess[str]:
    kw.setdefault("text", True)
    kw.setdefault("capture_output", True)
    return subprocess.run(cmd, check=kw.pop("check", False), **kw)


def log(msg: str) -> None:
    print(f"[package] {msg}", flush=True)


def target_dir() -> Path:
    p = run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=REPO)
    if p.returncode != 0:
        raise PackageError(f"cargo metadata failed:\n{p.stderr}")
    return Path(json.loads(p.stdout)["target_directory"])


def git_stamp() -> str | None:
    head = run(["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO)
    if head.returncode != 0 or not head.stdout.strip():
        return None
    status = run(
        ["git", "--no-optional-locks", "status", "--porcelain", "--untracked-files=no"],
        cwd=REPO,
    )
    if status.returncode != 0:
        return None
    stamp = head.stdout.strip()
    if status.stdout.strip():
        stamp += "-dirty"
    return stamp


def build_release() -> None:
    cmd = [
        "cargo",
        "build",
        "--locked",
        "--release",
        "-p",
        "dbsloc-cli",
        "-p",
        "dbsloc-app",
    ]
    env = {**os.environ}
    if sys.platform == "win32":
        env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"] = "link.exe"
        log("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=link.exe")
    if not env.get("DBSLOC_GIT_HASH", "").strip():
        stamp = git_stamp()
        if stamp:
            env["DBSLOC_GIT_HASH"] = stamp
            log(f"DBSLOC_GIT_HASH={stamp}")
    log(" ".join(cmd))
    subprocess.run(cmd, cwd=REPO, env=env, check=True)


def shipped_binaries(release_dir: Path) -> list[Path]:
    paths = [release_dir / exe_name(n) for n in (CLI, GUI)]
    missing = [p.name for p in paths if not p.is_file()]
    if missing:
        raise PackageError(
            f"missing release binaries in {release_dir}: {', '.join(missing)}"
        )
    return paths


def tracked_files(rel: str) -> list[str]:
    p = run(["git", "ls-files", "-z", "--", rel], cwd=REPO, encoding="utf-8")
    if p.returncode != 0:
        raise PackageError(f"git ls-files {rel} failed:\n{p.stderr}")
    files = [f for f in p.stdout.split("\0") if f]
    if not files:
        raise PackageError(f"no files tracked by git under {rel}")
    return files


def copy_tracked(rel: str, dst: Path) -> None:
    for f in tracked_files(rel):
        out = dst / Path(f).relative_to(rel)
        out.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO / f, out)


def assemble(release_dir: Path, out: Path, plat: str) -> Layout:
    name = f"dbsloc-{plat}"
    root = out / name
    if root.exists():
        shutil.rmtree(root)
    bin_dir = root if plat == "win" else root / "bin"
    bin_dir.mkdir(parents=True)
    resources = root / "resources"
    exes = []
    for src in shipped_binaries(release_dir):
        dst = bin_dir / src.name
        shutil.copy2(src, dst)
        exes.append(dst)
    for sub in ("ants", "dcm2niix"):
        copy_tracked(f"resources/{sub}/{plat}", resources / sub / plat)
    for sub in RESOURCE_DIRS:
        copy_tracked(f"resources/{sub}", resources / sub)
    for extra in ("README.md", "dcm2niix/LICENSE", "ants/ITK_THIRD_PARTY_LICENSES.md"):
        src = REPO / "resources" / extra
        if src.is_file():
            shutil.copy2(src, resources / extra)
    for extra in ("LICENSE", "NOTICE"):
        shutil.copy2(REPO / extra, root / extra)
    copy_tracked("LICENSES", root / "LICENSES")
    write_third_party_licenses(root / THIRD_PARTY_NAME)
    if plat == "win":
        ship_vc_runtime(exes)
    if plat != "win":
        ship_install_files(root, plat)
    (root / README_NAME).write_text(
        readme_text(plat, workspace_version()), encoding="utf-8"
    )
    archive = out / (f"{name}.zip" if plat == "win" else f"{name}.tar.gz")
    write_archive(root, archive)
    write_checksum(archive)
    return Layout(plat, root, bin_dir, resources, archive, exes)


def write_third_party_licenses(dst: Path) -> None:
    try:
        target = tpl.host_triple()
        bundle = tpl.collect(tpl.cargo_metadata(REPO, target, False), REPO, target)
    except tpl.LicenceError as e:
        raise PackageError(f"third-party licence bundle: {e}") from e
    dst.write_text(tpl.render(bundle), encoding="utf-8", newline="\n")
    log(f"wrote {dst.name} ({len(bundle.packages)} packages, target {target})")


def ship_install_files(root: Path, plat: str) -> None:
    script = root / INSTALL_SCRIPT
    shutil.copyfile(REPO / "scripts" / INSTALL_SCRIPT, script)
    script.chmod(0o755)
    if plat == "linux":
        shutil.copyfile(ICON_SRC, root / ICON_NAME)
        shutil.copyfile(DESKTOP_TEMPLATE_SRC, root / DESKTOP_TEMPLATE_NAME)


def ship_vc_runtime(exes: list[Path]) -> None:
    pool = REPO / "resources" / "dcm2niix" / "win"
    needed: set[str] = set()
    queue = list(exes)
    while queue:
        for dll in pe_imports(queue.pop()):
            low = dll.lower()
            if low.startswith(VC_REDIST_PREFIXES) and low not in needed:
                needed.add(low)
                src = pool / low
                if not src.is_file():
                    raise PackageError(f"{dll} is imported but not available in {pool}")
                queue.append(src)
    for low in sorted(needed):
        shutil.copy2(pool / low, exes[0].parent / low)
        log(f"ship {low} beside the executables")


def write_archive(root: Path, archive: Path) -> None:
    if archive.exists():
        archive.unlink()
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
            for p in sorted(root.rglob("*")):
                if p.is_file():
                    z.write(p, p.relative_to(root.parent).as_posix())
    else:
        with tarfile.open(archive, "w:gz", compresslevel=6) as t:
            t.add(root, arcname=root.name)
    log(f"wrote {archive} ({archive.stat().st_size / 1e6:.1f} MB)")


def write_checksum(archive: Path) -> Path:
    digest = hashlib.sha256()
    with archive.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    path = archive.with_name(archive.name + ".sha256")
    path.write_bytes(f"{digest.hexdigest()}  {archive.name}\n".encode())
    log(f"wrote {path}")
    return path


def readme_text(plat: str, version: str | None = None) -> str:
    version = version or workspace_version()
    gui = exe_name(GUI) if plat == "win" else f"bin/{GUI}"
    cli = exe_name(CLI) if plat == "win" else f"bin/{CLI}"
    layout = (
        "  dbs-localizer.exe        graphical application\n"
        "  dbsloc.exe               command-line tool\n"
        "  vcruntime140*.dll        Microsoft Visual C++ runtime for the executables above\n"
        "  resources/               templates, atlases, bundled ANTs and dcm2niix\n"
        if plat == "win"
        else "  bin/                     dbs-localizer (graphical application), dbsloc (command line)\n"
        "  resources/               templates, atlases, bundled ANTs and dcm2niix\n"
        "  install.sh               installer (works from this folder, no download)\n"
    )
    windows_notes = (
        "Windows SmartScreen\n"
        '  The executables are not code-signed. If Windows shows "Windows protected your PC",\n'
        '  click "More info" and then "Run anyway". Alternatively unblock the downloaded zip\n'
        "  before extracting it (right-click > Properties > Unblock, or in PowerShell:\n"
        "  Unblock-File dbsloc-win.zip).\n\n"
        if plat == "win"
        else ""
    )
    mac_notes = (
        "macOS Gatekeeper\n"
        "  The binaries are not notarised, and a copy downloaded in a browser is\n"
        "  quarantined. Run once: xattr -dr com.apple.quarantine <this folder>\n"
        "  (or right-click > Open). install.sh does this for its install folder.\n\n"
        if plat == "mac"
        else ""
    )
    linux_notes = (
        "Linux requirements\n"
        "  The archive is built on x86_64 against glibc and runs on current distributions\n"
        "  (Arch, Fedora, Ubuntu 24.04 or newer). The graphical application needs these\n"
        "  system libraries (the command line tool runs without them):\n"
        f"{LINUX_PACKAGE_HINTS}"
        "  A Vulkan-capable graphics driver (mesa or the vendor driver) enables GPU\n"
        "  registration and rendering; without one the software adapter is used.\n\n"
        if plat == "linux"
        else ""
    )
    if plat == "win":
        start = (
            "  Extract this folder anywhere (keep its contents together) and start "
            f"{gui}.\n"
        )
    else:
        menu = ", plus an application menu entry" if plat == "linux" else ""
        start = (
            "  Extract the archive (tar -xzf), then run ./install.sh. It installs under\n"
            f"  ~/.local (launchers in ~/.local/bin{menu}) and needs no network;\n"
            "  ./install.sh -u removes it again. Or skip the installation and start\n"
            f"  {gui} directly from this folder (keep its contents together).\n"
        )
    return (
        "Wyss DBS Localizer\n"
        "==================\n\n"
        "Getting started\n"
        f"{start}"
        f"  The command-line tool is {cli}; `{cli} --help` lists the commands and\n"
        f"  `{cli} run <bids_root> <subject>` runs the pipeline for one subject.\n\n"
        "Folder layout\n"
        f"{layout}\n"
        "  The executables look for the resources/ folder next to them, so do not move\n"
        "  them out of this folder. Downloaded templates go to the per-user data folder\n"
        "  (dbsloc template list shows where).\n\n"
        f"{windows_notes}"
        f"{mac_notes}"
        f"{linux_notes}"
        "Registration engine\n"
        "  Registration runs on the GPU when a usable graphics driver is installed; without\n"
        "  one the application still runs and registration falls back to the bundled ANTs\n"
        f"  binaries on the CPU (slower). `{cli} gpu probe` reports which engine will be used.\n\n"
        "Licence and source code\n"
        "  Wyss DBS Localizer is free software under the GNU General Public License\n"
        "  version 3 only; the full text is in LICENSE. The source code of this package\n"
        f"  is the tag {source_tag(version)} of the public repository {REPO_URL}\n"
        f"  (direct address: {source_url(version)}).\n"
        "  Licences of third-party components: NOTICE (bundled components),\n"
        "  THIRD_PARTY_LICENSES.md (licences of the Rust libraries built into the programs) and\n"
        "  LICENSES/ (full text of every licence that applies to a file in this package).\n"
    )


def pe_imports(path: Path) -> list[str]:
    data = path.read_bytes()
    (e_lfanew,) = struct.unpack_from("<I", data, 0x3C)
    if data[e_lfanew : e_lfanew + 4] != b"PE\0\0":
        raise PackageError(f"{path.name}: not a PE file")
    coff = e_lfanew + 4
    (n_sections,) = struct.unpack_from("<H", data, coff + 2)
    (opt_size,) = struct.unpack_from("<H", data, coff + 16)
    opt = coff + 20
    (magic,) = struct.unpack_from("<H", data, opt)
    plus = magic == 0x20B
    dir_base = opt + (112 if plus else 96)
    (image_base,) = struct.unpack_from(
        "<Q" if plus else "<I", data, opt + (24 if plus else 28)
    )
    sections = []
    sec = opt + opt_size
    for i in range(n_sections):
        off = sec + i * 40
        vsize, vaddr, rsize, raw = struct.unpack_from("<IIII", data, off + 8)
        sections.append((vaddr, max(vsize, rsize), raw))

    def rva(r: int) -> int:
        for vaddr, size, raw in sections:
            if vaddr <= r < vaddr + size:
                return raw + (r - vaddr)
        raise PackageError(f"{path.name}: RVA {r:#x} outside every section")

    def cstr(off: int) -> str:
        end = data.index(b"\0", off)
        return data[off:end].decode("ascii", "replace")

    names: list[str] = []
    imp_rva, _imp_size = struct.unpack_from("<II", data, dir_base + 1 * 8)
    if imp_rva:
        off = rva(imp_rva)
        while True:
            _oft, _ts, _fc, name_rva, _ft = struct.unpack_from("<IIIII", data, off)
            if name_rva == 0:
                break
            names.append(cstr(rva(name_rva)))
            off += 20
    delay_rva, _ = struct.unpack_from("<II", data, dir_base + 13 * 8)
    if delay_rva:
        off = rva(delay_rva)
        while True:
            attrs, name_rva = struct.unpack_from("<II", data, off)
            if name_rva == 0:
                break
            names.append(cstr(rva(name_rva if attrs & 1 else name_rva - image_base)))
            off += 32
    return names


def system32() -> Path:
    return Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32"


def check_dll_imports(layout: Layout) -> Check:
    problems: list[str] = []
    sys32 = system32()
    for pe in sorted(
        p for p in layout.root.rglob("*") if p.suffix.lower() in (".exe", ".dll")
    ):
        beside = {q.name.lower() for q in pe.parent.iterdir()}
        for dll in pe_imports(pe):
            low = dll.lower()
            if low.startswith(SYSTEM_DLL_PREFIXES):
                continue
            redist = low.startswith(VC_REDIST_PREFIXES)
            if not redist and (sys32 / low).is_file():
                continue
            if low in beside:
                continue
            problems.append(f"{pe.relative_to(layout.root)} -> {dll}")
    if problems:
        return Check("dll imports", False, "unresolved: " + "; ".join(problems))
    return Check(
        "dll imports",
        True,
        "every import is a Windows system DLL or ships beside the executable",
    )


LINUX_PACKAGE_HINTS = (
    "  Arch Linux:      sudo pacman -S --needed gtk3 vulkan-icd-loader libxkbcommon fontconfig\n"
    "  Debian / Ubuntu: sudo apt-get install libgtk-3-0 libvulkan1 libxkbcommon0 fontconfig\n"
    "  Fedora:          sudo dnf install gtk3 vulkan-loader libxkbcommon fontconfig\n"
)
BASE_SYSTEM_LIBS = (
    "libc.",
    "libm.",
    "libdl.",
    "libpthread.",
    "librt.",
    "ld-linux",
    "libgcc_s.",
    "linux-vdso",
)


def elf_shared_libs(path: Path) -> tuple[list[str], list[str]]:
    """Library names ldd resolves and names it reports as not found."""
    p = run(["ldd", str(path)], timeout=60)
    found: list[str] = []
    missing: list[str] = []
    for line in p.stdout.splitlines():
        parts = line.split()
        if not parts:
            continue
        if "not found" in line:
            missing.append(parts[0])
        elif "=>" in parts:
            found.append(parts[0])
    return found, missing


def glibc_requirement(path: Path) -> str:
    p = run(["objdump", "-T", str(path)], timeout=60)
    versions = {
        tuple(int(x) for x in m.group(1).split("."))
        for m in re.finditer(r"GLIBC_(\d+(?:\.\d+)+)", p.stdout)
    }
    return ".".join(str(x) for x in max(versions)) if versions else "unknown"


def check_shared_libs(layout: Layout) -> Check:
    problems: list[str] = []
    needed: set[str] = set()
    for exe in sorted(layout.bin_dir.iterdir()):
        if not exe.is_file() or exe.suffix:
            continue
        found, missing = elf_shared_libs(exe)
        problems.extend(f"{exe.name} -> {m}" for m in missing)
        needed.update(found)
    if problems:
        return Check("shared libraries", False, "not found: " + "; ".join(problems))
    extra = sorted(n for n in needed if not n.startswith(BASE_SYSTEM_LIBS))
    glibc = glibc_requirement(layout.exe(GUI))
    return Check(
        "shared libraries",
        True,
        f"glibc >= {glibc}; system libraries: {', '.join(extra) or 'none beyond libc'}",
    )


def check_readme_source(layout: Layout) -> Check:
    name = "README source code statement"
    readme = layout.root / README_NAME
    if not readme.is_file():
        return Check(name, False, f"{README_NAME} missing")
    text = readme.read_text(encoding="utf-8", errors="replace")
    needed = ("Licence and source code", f"{REPO_URL}/tree/v", "LICENSE")
    missing = [s for s in needed if s not in text]
    if missing:
        return Check(name, False, "missing: " + ", ".join(missing))
    return Check(name, True, "tag and repository address stated")


def check_install_script(layout: Layout) -> Check:
    script = layout.root / INSTALL_SCRIPT
    if not script.is_file():
        return Check("install.sh in archive", False, "missing")
    if not os.access(script, os.X_OK):
        return Check("install.sh in archive", False, "not executable")
    extra = [ICON_NAME, DESKTOP_TEMPLATE_NAME] if layout.platform == "linux" else []
    absent = [n for n in extra if not (layout.root / n).is_file()]
    if absent:
        return Check("install.sh in archive", False, f"missing: {', '.join(absent)}")
    return Check(
        "install.sh in archive", True, "executable, with its icon and menu entry"
    )


def sanitized_env(extra_path: list[Path] | None = None) -> dict[str, str]:
    keep = (
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
        "TMPDIR",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "HOME",
        "USERNAME",
        "USER",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "LANG",
        "LC_ALL",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "XDG_SESSION_TYPE",
        "DBUS_SESSION_BUS_ADDRESS",
        "SHELL",
        "TERM",
    )
    env = {k: v for k, v in os.environ.items() if k.upper() in keep}
    if sys.platform == "win32":
        root = os.environ.get("SystemRoot", r"C:\Windows")
        dirs = [Path(root) / "System32", Path(root), Path(root) / "System32" / "Wbem"]
    else:
        dirs = [
            Path(p)
            for p in ("/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin")
        ]
    env["PATH"] = os.pathsep.join(str(d) for d in [*dirs, *(extra_path or [])])
    return env


def tool_version(exe: Path, args: list[str], ok_codes: tuple[int, ...] = (0,)) -> Check:
    p = run([str(exe), *args], env=sanitized_env(), cwd=exe.parent, timeout=120)
    first = next(
        (ln.strip() for ln in (p.stdout + p.stderr).splitlines() if ln.strip()), ""
    )
    ok = p.returncode in ok_codes
    return Check(
        f"{exe.name} {' '.join(args)}",
        ok,
        first if ok else f"exit {p.returncode}: {first}",
    )


def write_nifti(path: Path, shape: tuple[int, int, int], voxel_mm: float, fill) -> None:
    nx, ny, nz = shape
    hdr = bytearray(348)
    struct.pack_into("<i", hdr, 0, 348)
    struct.pack_into("<8h", hdr, 40, 3, nx, ny, nz, 1, 1, 1, 1)
    struct.pack_into("<h", hdr, 70, 16)
    struct.pack_into("<h", hdr, 72, 32)
    struct.pack_into("<8f", hdr, 76, 1.0, voxel_mm, voxel_mm, voxel_mm, 0, 0, 0, 0)
    struct.pack_into("<f", hdr, 108, 352.0)
    struct.pack_into("<f", hdr, 112, 1.0)
    struct.pack_into("<c", hdr, 123, b"\x0a")
    struct.pack_into("<hh", hdr, 252, 1, 1)
    origin = [-voxel_mm * (n - 1) / 2 for n in shape]
    struct.pack_into("<6f", hdr, 256, 0, 0, 0, *origin)
    struct.pack_into("<4f", hdr, 280, voxel_mm, 0, 0, origin[0])
    struct.pack_into("<4f", hdr, 296, 0, voxel_mm, 0, origin[1])
    struct.pack_into("<4f", hdr, 312, 0, 0, voxel_mm, origin[2])
    hdr[344:348] = b"n+1\0"
    body = bytearray()
    for k in range(nz):
        for j in range(ny):
            for i in range(nx):
                body += struct.pack("<f", fill(i, j, k))
    path.parent.mkdir(parents=True, exist_ok=True)
    with gzip.open(path, "wb") as f:
        f.write(bytes(hdr) + b"\0\0\0\0" + bytes(body))


def write_tiny_bids(root: Path) -> None:
    n = 24
    c = (n - 1) / 2

    def sphere(i: int, j: int, k: int) -> float:
        r2 = (i - c) ** 2 + (j - c) ** 2 + (k - c) ** 2
        return 100.0 + 0.5 * i + (400.0 if r2 < (0.4 * n) ** 2 else 0.0)

    anat = root / "rawdata" / "sub-01"
    write_nifti(
        anat / "ses-preop" / "anat" / "sub-01_ses-preop_T1w.nii.gz",
        (n, n, n),
        2.0,
        sphere,
    )
    write_nifti(
        anat / "ses-postop" / "anat" / "sub-01_ses-postop_CT.nii.gz",
        (n, n, n),
        2.0,
        sphere,
    )
    (root / "rawdata" / "dataset_description.json").write_text(
        json.dumps({"Name": "package smoke", "BIDSVersion": "1.9.0"}), encoding="utf-8"
    )


def run_preprocess(
    cli: Path, bids: Path, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    env = dict(env, ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS="2")
    cmd = [
        str(cli),
        "run",
        str(bids),
        "01",
        "--stages",
        "preprocess",
        "--reg-backend",
        "ants",
        "--headless",
        "--no-checkreg",
        "--no-index",
        "--log",
        "quiet",
    ]
    return run(cmd, env=env, cwd=bids, timeout=600)


def stage_result(p: subprocess.CompletedProcess[str], stage: str) -> str:
    try:
        payload = json.loads(p.stdout)
    except json.JSONDecodeError:
        return f"no JSON summary (exit {p.returncode}): {p.stderr.strip()[-400:]}"
    for row in payload.get("results", []):
        if row.get("stage") == stage:
            return str(row.get("result"))
    return f"stage {stage} missing from the summary"


def inside(path: str, root: Path) -> bool:
    try:
        Path(path).resolve().relative_to(root.resolve())
        return True
    except ValueError:
        return False


def check_template_resolution(layout: Layout) -> Check:
    p = run(
        [str(layout.exe(CLI)), "template", "list", "--json"],
        env=sanitized_env(),
        cwd=layout.root,
    )
    if p.returncode != 0:
        return Check(
            "templates from package", False, f"exit {p.returncode}: {p.stderr.strip()}"
        )
    rows = json.loads(p.stdout)
    bundled = [r for r in rows if r["status"] == "bundled"]
    if not bundled:
        return Check(
            "templates from package", False, f"no bundled template: {json.dumps(rows)}"
        )
    bad = [
        r["path"]
        for r in bundled
        if not r["path"] or not inside(r["path"], layout.root)
    ]
    if bad:
        return Check("templates from package", False, f"outside the package: {bad}")
    return Check("templates from package", True, bundled[0]["path"])


def check_ants_resolution(layout: Layout, tmp: Path) -> list[Check]:
    checks = []
    bids = tmp / "bids_ok"
    write_tiny_bids(bids)
    p = run_preprocess(layout.exe(CLI), bids, sanitized_env())
    res = stage_result(p, "preprocess")
    checks.append(
        Check("preprocess (N4) with package ANTs", res.startswith("ok:"), res)
    )
    plat_dir = layout.resources / "ants" / layout.platform
    hidden = plat_dir.with_name(plat_dir.name + ".hidden")
    plat_dir.rename(hidden)
    try:
        bids2 = tmp / "bids_noants"
        write_tiny_bids(bids2)
        p = run_preprocess(layout.exe(CLI), bids2, sanitized_env())
        res = stage_result(p, "preprocess")
        checks.append(
            Check(
                "preprocess fails once package ANTs is hidden",
                "N4BiasFieldCorrection not found" in res,
                res,
            )
        )
    finally:
        hidden.rename(plat_dir)
    return checks


def check_viz_resolution(layout: Layout) -> Check:
    bad_manifest = layout.root / "no_such_manifest.json"
    p = run(
        [
            str(layout.exe(CLI)),
            "viz",
            "scene",
            "--manifest",
            str(bad_manifest),
            "--headless-check",
        ],
        env=sanitized_env(),
        cwd=layout.root,
    )
    text = p.stdout + p.stderr
    if p.returncode != 1 or "scene manifest" not in text:
        return Check(
            "viewer binary beside dbsloc",
            False,
            f"exit {p.returncode}: {text.strip()[-300:]}",
        )
    return Check(
        "viewer binary beside dbsloc",
        True,
        "dbsloc started dbs-localizer, which rejected the missing manifest",
    )


def check_gpu_probe(layout: Layout) -> Check:
    p = run(
        [str(layout.exe(CLI)), "gpu", "probe"],
        env=sanitized_env(),
        cwd=layout.root,
        timeout=300,
    )
    lines = [
        ln.strip()
        for ln in p.stdout.splitlines()
        if ln.startswith(("adapter", "verdict", "auto selects"))
    ]
    return Check(
        "dbsloc gpu probe",
        p.returncode in (0, 1),
        "; ".join(lines) or p.stderr.strip()[-200:],
    )


def check_gui_smoke(layout: Layout, tmp: Path) -> Check:
    png = tmp / "gui_smoke.png"
    env = dict(sanitized_env(), DBSLOC_APP_SCREENSHOT_PNG=str(png))
    try:
        p = run([str(layout.exe(GUI))], env=env, cwd=layout.root, timeout=300)
    except subprocess.TimeoutExpired:
        return Check("gui screenshot smoke", False, "timed out after 300 s")
    if p.returncode != 0:
        return Check(
            "gui screenshot smoke",
            False,
            f"exit {p.returncode}: {p.stderr.strip()[-400:]}",
        )
    if not png.is_file() or png.stat().st_size == 0:
        return Check(
            "gui screenshot smoke", False, f"no PNG written: {p.stderr.strip()[-400:]}"
        )
    if png.read_bytes()[:8] != b"\x89PNG\r\n\x1a\n":
        return Check("gui screenshot smoke", False, "output is not a PNG")
    return Check("gui screenshot smoke", True, f"{png.stat().st_size} bytes")


def check_e2e(layout: Layout) -> Check:
    uv = shutil.which("uv")
    if uv is None:
        return Check("synthetic end-to-end", False, "uv not found on PATH")
    env = sanitized_env([Path(uv).parent])
    env.update(
        DBSLOC_BIN=str(layout.exe(CLI)),
        DBSLOC_REG_BACKEND="ants",
        ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS="4",
    )
    for k in ("UV_CACHE_DIR", "UV_PYTHON", "UV_PROJECT_ENVIRONMENT"):
        if k in os.environ:
            env[k] = os.environ[k]
    t0 = time.monotonic()
    p = run(
        [
            uv,
            "run",
            "--no-sync",
            "pytest",
            "tests/test_e2e_synthetic.py",
            "-q",
            "-rs",
            "-p",
            "no:cacheprovider",
        ],
        env=env,
        cwd=REPO,
        timeout=3600,
    )
    tail = "\n".join((p.stdout + p.stderr).strip().splitlines()[-6:])
    summary = next(
        (
            ln
            for ln in reversed(p.stdout.splitlines())
            if "passed" in ln or "failed" in ln or "skipped" in ln
        ),
        "",
    )
    ok = p.returncode == 0 and "passed" in summary and "skipped" not in summary
    return Check(
        "synthetic end-to-end",
        ok,
        f"{summary} ({time.monotonic() - t0:.0f} s)" if ok else tail,
    )


def extract(archive: Path, dest: Path) -> Path:
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as z:
            z.extractall(dest)
    else:
        with tarfile.open(archive) as t:
            t.extractall(dest, filter="data")
    (root,) = [p for p in dest.iterdir() if p.is_dir()]
    return root


def extracted_layout(built: Layout, root: Path) -> Layout:
    bin_dir = root if built.platform == "win" else root / "bin"
    return Layout(
        built.platform,
        root,
        bin_dir,
        root / "resources",
        built.archive,
        [bin_dir / e.name for e in built.exes],
    )


def test_archive(built: Layout, gui_smoke: bool, e2e: bool) -> list[Check]:
    checks: list[Check] = []
    with tempfile.TemporaryDirectory(prefix="dbsloc_pkg_") as td:
        tmp = Path(td)
        layout = extracted_layout(built, extract(built.archive, tmp / "extract"))
        log(f"testing {layout.root}")
        if layout.platform == "win":
            checks.append(check_dll_imports(layout))
        if layout.platform == "linux":
            checks.append(check_shared_libs(layout))
        if layout.platform != "win":
            checks.append(check_install_script(layout))
        checks.append(check_readme_source(layout))
        checks.append(tool_version(layout.exe(CLI), ["--version"]))
        checks.append(check_template_resolution(layout))
        checks.extend(check_ants_resolution(layout, tmp))
        checks.append(check_viz_resolution(layout))
        checks.append(check_gpu_probe(layout))
        ants_dir = layout.resources / "ants" / layout.platform
        for name in ANTS:
            flag = "--help" if name == "antsApplyTransforms" else "--version"
            checks.append(tool_version(ants_dir / exe_name(name), [flag]))
        checks.append(
            tool_version(
                layout.resources / "dcm2niix" / layout.platform / exe_name("dcm2niix"),
                ["-v"],
                (0, 3),
            )
        )
        if e2e:
            checks.append(check_e2e(layout))
        if gui_smoke:
            checks.append(check_gui_smoke(layout, tmp))
    return checks


def print_table(checks: list[Check], archive: Path) -> None:
    width = max(len(c.name) for c in checks)
    print()
    print(f"archive: {archive} ({archive.stat().st_size / 1e6:.1f} MB)")
    for c in checks:
        status = "PASS" if c.ok else "FAIL"
        print(f"  {status}  {c.name:<{width}}  {c.detail}")
    n_fail = sum(not c.ok for c in checks)
    print(f"\n{len(checks) - n_fail} passed, {n_fail} failed")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--skip-build",
        action="store_true",
        help="use the existing target/release binaries",
    )
    ap.add_argument(
        "--no-gui-smoke",
        action="store_true",
        help="skip the GUI screenshot smoke test (no display / GPU)",
    )
    ap.add_argument(
        "--e2e",
        action="store_true",
        help="also run tests/test_e2e_synthetic.py against the packaged dbsloc",
    )
    ap.add_argument(
        "--out",
        type=Path,
        default=REPO / "dist",
        help="output directory (default: dist/)",
    )
    args = ap.parse_args(argv)
    plat = host_platform()
    if plat == "mac" and platform.machine().lower() not in ("arm64", "aarch64"):
        raise PackageError("the bundled macOS ANTs binaries are arm64 only")
    if not args.skip_build:
        build_release()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    built = assemble(target_dir() / "release", out, plat)
    checks = test_archive(built, gui_smoke=not args.no_gui_smoke, e2e=args.e2e)
    print_table(checks, built.archive)
    return 0 if all(c.ok for c in checks) else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except PackageError as e:
        print(f"[package] error: {e}", file=sys.stderr)
        sys.exit(2)
