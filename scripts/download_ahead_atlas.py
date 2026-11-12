# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Download the CC BY 4.0 AHEAD MNI09b subcortical structure masks."""

from __future__ import annotations

import argparse
import hashlib
import shutil
import tarfile
import tempfile
import urllib.request
from pathlib import Path

URL = "https://ndownloader.figshare.com/files/21209229"
MD5 = "024d96847765af480dfd69612011684c"
ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "resources" / "atlases" / "ahead" / "structures_mni09b"
ARCHIVE = ROOT / "resources" / "atlases" / "ahead" / "structures_mni09b.tar.gz"


def _md5(path: Path) -> str:
    h = hashlib.md5(usedforsecurity=False)
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def _safe_extract(archive: Path, dest: Path) -> None:
    dest = dest.resolve()
    with tarfile.open(archive, "r:gz") as tf:
        for member in tf.getmembers():
            target = (dest / member.name).resolve()
            if not str(target).startswith(str(dest)):
                raise RuntimeError(f"unsafe archive path: {member.name}")
        tf.extractall(dest)


def _copy_masks(src: Path, dst: Path) -> int:
    dst.mkdir(parents=True, exist_ok=True)
    n = 0
    for p in src.rglob("*"):
        if (
            p.is_file()
            and (p.name.endswith(".nii") or p.name.endswith(".nii.gz"))
            and "_mask-" in p.name
        ):
            shutil.copy2(p, dst / p.name)
            n += 1
    return n


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--force", action="store_true", help="redownload and replace installed files"
    )
    ap.add_argument(
        "--archive",
        type=Path,
        help="use an already-downloaded structures_mni09b.tar.gz archive",
    )
    args = ap.parse_args()

    if OUT_DIR.is_dir() and any(OUT_DIR.iterdir()) and not args.force:
        print(f"AHEAD already installed: {OUT_DIR}")
        return 0

    ARCHIVE.parent.mkdir(parents=True, exist_ok=True)
    if args.archive is not None:
        shutil.copy2(args.archive.expanduser(), ARCHIVE)
    elif args.force or not ARCHIVE.is_file():
        print(f"Downloading {URL}")
        urllib.request.urlretrieve(URL, ARCHIVE)

    got = _md5(ARCHIVE)
    if got != MD5:
        raise RuntimeError(
            f"checksum mismatch for {ARCHIVE}: got {got}, expected {MD5}"
        )

    with tempfile.TemporaryDirectory(prefix="dbsloc_ahead_") as td:
        tmp = Path(td)
        _safe_extract(ARCHIVE, tmp)
        if OUT_DIR.exists():
            shutil.rmtree(OUT_DIR)
        n = _copy_masks(tmp, OUT_DIR)
        if n == 0:
            raise RuntimeError("archive contained no mask NIfTI files")
    ARCHIVE.unlink(missing_ok=True)

    print(f"Installed {n} AHEAD mask NIfTIs: {OUT_DIR}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
