# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Build the per-user Windows installer `dbs-localizer-setup.exe` (WiX 7 Burn bundle around an MSI).

Usage: python scripts/build_installer.py [--stage DIR] [--out DIR] [--version X.Y.Z] [--test]

The payload is the staged folder written by `scripts/package_release.py` (default
`dist/dbsloc-win`). `--test` installs the result silently for the current user, checks it
and uninstalls it again (needs no elevation, changes only HKCU and the user profile).
Exit 1 on any failure.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
REPO_URL = "https://github.com/WyssCenter/dbs-localizer"
WIX_DIR = REPO / "packaging" / "wix"
ICON = REPO / "crates" / "dbsloc-viz" / "assets" / "brand" / "dbs-loc-icon.ico"
BA_EXTENSION = "WixToolset.BootstrapperApplications.wixext/7.0.0"
SETUP_NAME = "dbs-localizer-setup.exe"
PRODUCT_NAME = "Wyss DBS Localizer"
SHORTCUT_NAME = "DBS Localizer.lnk"
VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$")
MSI_LIMITS = (255, 255, 65535)
REQUIRED_PAYLOAD = (
    "dbs-localizer.exe",
    "dbsloc.exe",
    "resources/ants/win/antsRegistration.exe",
    "resources/templates",
    "LICENSE",
    "NOTICE",
    "THIRD_PARTY_LICENSES.md",
    "README.txt",
    "LICENSES",
    "LICENSES/AGPL-3.0-only.txt",
)
AGPL_TEXT = "LICENSES/AGPL-3.0-only.txt"


def log(message: str) -> None:
    print(f"build_installer: {message}", flush=True)


def fail(message: str) -> None:
    print(f"build_installer: error: {message}", file=sys.stderr)
    sys.exit(1)


def workspace_version() -> str:
    data = tomllib.loads((REPO / "Cargo.toml").read_text(encoding="utf-8"))
    return str(data["workspace"]["package"]["version"])


def msi_version(version: str) -> str:
    """Return `major.minor.patch` for an MSI: pre-release and build suffixes are dropped and the
    fields are checked against the MSI limits (255.255.65535)."""
    match = VERSION_RE.match(version.strip())
    if match is None:
        raise ValueError(f"not a version of the form X.Y.Z: {version!r}")
    fields = [int(part) for part in match.groups()]
    for name, value, limit in zip(("major", "minor", "patch"), fields, MSI_LIMITS):
        if value > limit:
            raise ValueError(f"{name} field {value} exceeds the MSI limit {limit}")
    return ".".join(str(value) for value in fields)


def license_rtf(text: str) -> str:
    """Convert plain text to a minimal RTF document (escapes, non-ASCII as `\\uN?`, `\\par` lines)."""
    out: list[str] = []
    for char in text.replace("\r\n", "\n").replace("\r", "\n"):
        if char == "\\":
            out.append("\\\\")
        elif char == "{":
            out.append("\\{")
        elif char == "}":
            out.append("\\}")
        elif char == "\n":
            out.append("\\par\n")
        elif char == "\t":
            out.append("\\tab ")
        elif ord(char) < 128:
            out.append(char)
        else:
            code = ord(char)
            if code < 0x10000:
                units = [code]
            else:
                code -= 0x10000
                units = [0xD800 + (code >> 10), 0xDC00 + (code & 0x3FF)]
            for unit in units:
                signed = unit - 0x10000 if unit > 32767 else unit
                out.append(f"\\u{signed}?")
    body = "".join(out)
    return (
        "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\fswiss Segoe UI;}}\\f0\\fs20\n"
        + body
        + "}"
    )


def run(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )


def check_tooling() -> str:
    wix = shutil.which("wix")
    if wix is None:
        fail(
            "`wix` not found on PATH; install it with: "
            "dotnet tool install --global wix --version 7.0.0"
        )
    listing = run([wix, "extension", "list", "-g"])
    if BA_EXTENSION.replace("/", " ") not in listing.stdout:
        fail(
            f"WiX extension missing; install it with: wix extension add -g {BA_EXTENSION}"
        )
    return wix


def check_payload(stage: Path) -> None:
    missing = [rel for rel in REQUIRED_PAYLOAD if not (stage / rel).exists()]
    if missing:
        fail(f"payload {stage} is missing: {', '.join(missing)}")


def wix_build(wix: str, args: list[str]) -> None:
    result = run([wix, "build", *args])
    output = (result.stdout or "") + (result.stderr or "")
    if "WIX7015" in output:
        fail("the WiX 7 EULA is not accepted; read it and run: wix eula accept wix7")
    if output.strip():
        print(output.rstrip(), flush=True)
    if result.returncode != 0:
        fail(f"wix build failed with exit code {result.returncode}")


def write_checksum(path: Path) -> Path:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    checksum = path.with_name(path.name + ".sha256")
    checksum.write_bytes(f"{digest.hexdigest()}  {path.name}\n".encode())
    log(f"wrote {checksum}")
    return checksum


def build(stage: Path, out: Path, version: str) -> Path:
    check_payload(stage)
    wix = check_tooling()
    try:
        product_version = msi_version(version)
    except ValueError as err:
        fail(str(err))
    out.mkdir(parents=True, exist_ok=True)
    work = out / "installer-build"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)

    notice = (stage / "NOTICE").read_text(encoding="utf-8", errors="replace")
    license_text = (stage / "LICENSE").read_text(encoding="utf-8", errors="replace")
    agpl_text = (stage / AGPL_TEXT).read_text(encoding="utf-8", errors="replace")
    rtf = work / "license.rtf"
    rtf.write_text(
        license_rtf(license_text + "\n\n" + notice + "\n\n" + agpl_text),
        encoding="ascii",
    )

    msi = work / "dbs-localizer.msi"
    log(f"building MSI {product_version}")
    wix_build(
        wix,
        [
            str(WIX_DIR / "Product.wxs"),
            "-arch",
            "x64",
            "-d",
            f"ProductVersion={product_version}",
            "-d",
            f"RepoUrl={REPO_URL}",
            "-d",
            f"StageDir={stage}",
            "-d",
            f"IconPath={ICON}",
            "-pdbtype",
            "none",
            "-o",
            str(msi),
        ],
    )
    setup = out / SETUP_NAME
    log("building bundle")
    wix_build(
        wix,
        [
            str(WIX_DIR / "Bundle.wxs"),
            "-arch",
            "x64",
            "-ext",
            BA_EXTENSION,
            "-d",
            f"ProductVersion={product_version}",
            "-d",
            f"MsiPath={msi}",
            "-d",
            f"IconPath={ICON}",
            "-d",
            f"LicensePath={rtf}",
            "-pdbtype",
            "none",
            "-o",
            str(setup),
        ],
    )
    shutil.rmtree(work, ignore_errors=True)
    write_checksum(setup)
    log(f"built {setup} ({setup.stat().st_size / 1e6:.1f} MB)")
    return setup


def installed_dir() -> Path | None:
    import winreg

    base = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
    try:
        root = winreg.OpenKey(winreg.HKEY_CURRENT_USER, base)
    except OSError:
        return None
    with root:
        index = 0
        while True:
            try:
                name = winreg.EnumKey(root, index)
            except OSError:
                return None
            index += 1
            try:
                with winreg.OpenKey(root, name) as key:
                    if winreg.QueryValueEx(key, "DisplayName")[0] != PRODUCT_NAME:
                        continue
                    location = winreg.QueryValueEx(key, "InstallLocation")[0]
                    if location:
                        return Path(location)
            except OSError:
                continue


def known_install_dir() -> Path:
    return Path(os.environ["LOCALAPPDATA"]) / "Programs" / PRODUCT_NAME


def user_path() -> str:
    import winreg

    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment") as key:
        try:
            return str(winreg.QueryValueEx(key, "Path")[0])
        except OSError:
            return ""


def path_entries(value: str) -> list[str]:
    return [part for part in value.split(";") if part]


def path_has(directory: Path) -> bool:
    wanted = str(directory).rstrip("\\").lower()
    return any(
        part.rstrip("\\").lower() == wanted for part in path_entries(user_path())
    )


def shortcut_paths() -> list[Path]:
    import winreg

    appdata = Path(os.environ["APPDATA"])
    desktop = Path(os.environ.get("USERPROFILE", "")) / "Desktop"
    try:
        with winreg.OpenKey(
            winreg.HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders",
        ) as key:
            desktop = Path(
                os.path.expandvars(str(winreg.QueryValueEx(key, "Desktop")[0]))
            )
    except OSError:
        pass
    return [
        appdata / "Microsoft" / "Windows" / "Start Menu" / "Programs" / SHORTCUT_NAME,
        desktop / SHORTCUT_NAME,
    ]


def tail(path: Path, lines: int = 40) -> str:
    chunks = []
    for candidate in sorted(path.parent.glob(path.stem + "*")):
        try:
            raw = candidate.read_bytes()
        except OSError:
            continue
        encoding = "utf-16" if raw[:2] in (b"\xff\xfe", b"\xfe\xff") else "utf-8"
        text = raw.decode(encoding, "replace")
        chunks.append(f"--- {candidate.name}\n" + "\n".join(text.splitlines()[-lines:]))
    return "\n".join(chunks)


def run_tests(setup: Path, version: str) -> int:
    results: list[tuple[str, bool, str]] = []

    def record(name: str, ok: bool, detail: str = "") -> None:
        results.append((name, ok, detail))

    logdir = Path(tempfile.mkdtemp(prefix="dbsloc-setup-"))
    install_log = logdir / "install.log"
    uninstall_log = logdir / "uninstall.log"
    path_before = user_path()
    log(f"user PATH before: {path_before}")

    install = run([str(setup), "/quiet", "/norestart", "/log", str(install_log)])
    record("install exits 0", install.returncode == 0, f"exit {install.returncode}")

    registry_dir = installed_dir()
    known = known_install_dir()
    where = registry_dir or known
    log(
        f"install dir from uninstall registry: {registry_dir}; known per-user path: {known}"
    )
    record("install dir exists", where.is_dir(), str(where))

    dbsloc = where / "dbsloc.exe"
    if dbsloc.is_file():
        ver = run([str(dbsloc), "--version"])
        record(
            "dbsloc --version",
            ver.returncode == 0 and msi_version(version) in ver.stdout,
            ver.stdout.strip(),
        )
        listing = run([str(dbsloc), "template", "list", "--json"])
        inside = False
        detail = f"exit {listing.returncode}"
        if listing.returncode == 0:
            try:
                bundled = [
                    t
                    for t in json.loads(listing.stdout)
                    if t.get("status") == "bundled"
                ]
                root = str(where).lower()
                inside = bool(bundled) and all(
                    str(t["path"]).lower().startswith(root) for t in bundled
                )
                detail = str(bundled[0]["path"]) if bundled else "no bundled template"
            except (ValueError, KeyError) as err:
                detail = f"bad json: {err}"
        record("bundled template inside install dir", inside, detail)
    else:
        record("dbsloc --version", False, "dbsloc.exe missing")
        record("bundled template inside install dir", False, "dbsloc.exe missing")

    for shortcut in shortcut_paths():
        record(f"shortcut {shortcut.parent.name}", shortcut.is_file(), str(shortcut))
    record("user PATH contains install dir", path_has(where))

    uninstall = run(
        [str(setup), "/uninstall", "/quiet", "/norestart", "/log", str(uninstall_log)]
    )
    record(
        "uninstall exits 0", uninstall.returncode == 0, f"exit {uninstall.returncode}"
    )
    record("install dir removed", not where.exists(), str(where))
    for shortcut in shortcut_paths():
        record(
            f"shortcut removed {shortcut.parent.name}",
            not shortcut.exists(),
            str(shortcut),
        )
    record("user PATH entry removed", not path_has(where))
    path_after = user_path()
    log(f"user PATH after: {path_after}")
    record("user PATH restored", path_entries(path_after) == path_entries(path_before))

    width = max(len(name) for name, _, _ in results)
    failed = False
    for name, ok, detail in results:
        failed |= not ok
        print(f"{'PASS' if ok else 'FAIL'}  {name.ljust(width)}  {detail}")
    if failed:
        print(tail(install_log))
        print(tail(uninstall_log))
        return 1
    shutil.rmtree(logdir, ignore_errors=True)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", type=Path, default=REPO / "dist" / "dbsloc-win")
    parser.add_argument("--out", type=Path, default=REPO / "dist")
    parser.add_argument("--version", default=None)
    parser.add_argument("--test", action="store_true")
    args = parser.parse_args(argv)
    if sys.platform != "win32":
        fail("the installer can only be built on Windows")
    version = args.version or workspace_version()
    setup = build(args.stage.resolve(), args.out.resolve(), version)
    if args.test:
        return run_tests(setup, version)
    return 0


if __name__ == "__main__":
    sys.exit(main())
