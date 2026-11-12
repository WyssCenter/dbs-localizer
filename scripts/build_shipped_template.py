# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

import nibabel as nib
import numpy as np

ENTRY_ID = "MNI152NLin2009bAsym_res-1mm"
SPACE = "MNI152NLin2009bAsym"
SOURCE_URL = "https://www.bic.mni.mcgill.ca/~vfonov/icbm/2009/mni_icbm152_nlin_asym_09b_nifti.zip"
ZIP_SHA256 = "5759d292e531ad600dafd988955b014dead3156643361bec624793c41611c1ab"
MEMBERS = {
    "t1": "mni_icbm152_nlin_asym_09b/mni_icbm152_t1_tal_nlin_asym_09b_hires.nii",
    "t2": "mni_icbm152_nlin_asym_09b/mni_icbm152_t2_tal_nlin_asym_09b_hires.nii",
}
LICENCE_MEMBER = "COPYING"
MASK_ZIP_SHA256 = "804382e5d68a42ff8096ee845a7600b6658434da762fa67840a149eb25d48bd4"
MASK_MEMBER = "mni_icbm152_nlin_asym_09c/mni_icbm152_t1_tal_nlin_asym_09c_mask.nii"
MASK_SPACE = "MNI152NLin2009cAsym"
MASK_THRESHOLD = 0.5
BRIDGE_PREFIX = f"from-{SPACE}_"
METHOD = "decimation by 2 on the stored int16 grid (identical to resample.rs at 1.0 mm)"


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def decimate(raw: bytes) -> tuple[bytes, tuple[int, ...]]:
    img = nib.Nifti1Image.from_bytes(raw)
    stored = np.asarray(img.dataobj.get_unscaled())
    if stored.dtype != np.int16:
        raise SystemExit(f"expected an int16 template, got {stored.dtype}")
    small = np.ascontiguousarray(stored[::2, ::2, ::2])
    hdr = img.header.copy()
    affine = np.array(img.affine, dtype=np.float64)
    affine[:3, :3] *= 2.0
    sform_code = int(hdr["sform_code"])
    qform_code = int(hdr["qform_code"])
    hdr.set_slope_inter(img.dataobj.slope, img.dataobj.inter)
    hdr.set_data_shape(small.shape)
    zooms = [float(z) * 2.0 for z in hdr.get_zooms()[:3]]
    hdr.set_zooms(zooms)
    hdr.set_sform(affine, code=sform_code)
    hdr.set_qform(affine, code=qform_code)
    hdr.set_data_offset(352)
    buf = io.BytesIO()
    hdr.write_to(buf)
    if buf.tell() != 352:
        raise SystemExit(f"unexpected header size {buf.tell()}")
    buf.write(small.astype(hdr.get_data_dtype(), copy=False).tobytes(order="F"))
    return buf.getvalue(), small.shape


def write_gz(path: Path, payload: bytes) -> None:
    with (
        path.open("wb") as raw,
        gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
        ) as gz,
    ):
        gz.write(payload)


def find_ants(root: Path) -> Path:
    exe = "antsApplyTransforms.exe" if os.name == "nt" else "antsApplyTransforms"
    platform = (
        "win" if os.name == "nt" else ("mac" if sys.platform == "darwin" else "linux")
    )
    bundled = root / "resources/ants" / platform / exe
    if not bundled.is_file():
        raise SystemExit(f"error: {bundled} not found")
    return bundled


def user_data_dir() -> Path:
    def nonempty(name: str) -> Path | None:
        value = os.environ.get(name, "").strip()
        return Path(value) if value else None

    override = nonempty("DBSLOC_DATA_DIR")
    if override is not None:
        return override
    if os.name == "nt":
        appdata = nonempty("APPDATA")
        if appdata is None:
            raise SystemExit("error: APPDATA is not set; pass --bridge-dir")
        return appdata / "dbsloc"
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support/dbsloc"
    return (nonempty("XDG_DATA_HOME") or Path.home() / ".local/share") / "dbsloc"


def default_bridge_dir() -> Path:
    base = user_data_dir()
    matches = sorted((base / "templates/bridges").glob(f"{MASK_SPACE}__*"))
    if len(matches) != 1:
        raise SystemExit(
            f"error: expected exactly one {MASK_SPACE} bridge under {base}, found {len(matches)}; "
            "pass --bridge-dir"
        )
    return matches[0]


def build_mask(
    zip_path: Path, bridge_dir: Path, out: Path, root: Path
) -> tuple[dict, dict, str]:
    zip_hash = sha256_file(zip_path)
    if zip_hash != MASK_ZIP_SHA256:
        raise SystemExit(
            f"error: {zip_path} sha256 {zip_hash} != pinned {MASK_ZIP_SHA256}"
        )
    inverse = bridge_dir / f"{BRIDGE_PREFIX}InverseComposite.h5"
    record = bridge_dir / "bridge.json"
    for needed in (inverse, record):
        if not needed.is_file():
            raise SystemExit(f"error: {needed} not found")
    reference = out / "t1.nii.gz"
    with zipfile.ZipFile(zip_path) as zf:
        raw = zf.read(MASK_MEMBER)
    ants = find_ants(root)
    env = dict(os.environ, ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS="4")
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / "mask09c.nii"
        warped = Path(tmp) / "warped.nii.gz"
        src.write_bytes(raw)
        subprocess.run(
            [
                str(ants),
                "-d",
                "3",
                "-i",
                str(src),
                "-r",
                str(reference),
                "-t",
                str(inverse),
                "-n",
                "Linear",
                "-o",
                str(warped),
            ],
            check=True,
            env=env,
        )
        warped_img = nib.load(str(warped))
        data = np.asarray(warped_img.dataobj, dtype=np.float32)
    ref = nib.load(str(reference))
    if data.shape != ref.shape:
        raise SystemExit(f"error: warped mask shape {data.shape} != {ref.shape}")
    binary = (data > MASK_THRESHOLD).astype(np.uint8)
    hdr = ref.header.copy()
    hdr.set_data_dtype(np.uint8)
    hdr.set_slope_inter(1.0, 0.0)
    hdr.set_data_offset(352)
    img = nib.Nifti1Image(binary, ref.affine, hdr)
    img.set_sform(ref.affine, code=int(ref.header["sform_code"]))
    img.set_qform(ref.affine, code=int(ref.header["qform_code"]))
    buf = io.BytesIO()
    img.header.write_to(buf)
    if buf.tell() != 352:
        raise SystemExit(f"unexpected header size {buf.tell()}")
    buf.write(np.asarray(binary, dtype=np.uint8).tobytes(order="F"))
    target = out / "brainmask.nii.gz"
    write_gz(target, buf.getvalue())
    print(f"{target}: shape {binary.shape}, {int(binary.sum())} voxels")
    member = {"member": MASK_MEMBER, "sha256": sha256_bytes(raw)}
    entry = {"sha256": sha256_file(target), "shape": list(binary.shape)}
    method = (
        f"{MASK_SPACE} brain mask (zip sha256 {MASK_ZIP_SHA256}) resampled onto the "
        f"{SPACE} 1.0 mm grid with antsApplyTransforms through the bridge "
        f"{BRIDGE_PREFIX}InverseComposite.h5 (bridge.json sha256 {sha256_file(record)}), "
        f"linear interpolation, threshold {MASK_THRESHOLD}, uint8 0/1"
    )
    return member, entry, method


def carry_over_mask(out: Path, template_json: dict) -> None:
    previous_path = out / "template.json"
    mask_path = out / "brainmask.nii.gz"
    if not previous_path.is_file() or not mask_path.is_file():
        return
    previous = json.loads(previous_path.read_text(encoding="utf-8"))
    member = previous.get("members", {}).get("mask")
    entry = previous.get("files", {}).get("brainmask.nii.gz")
    method = previous.get("mask_method")
    if member is None or entry is None or method is None:
        return
    if entry.get("sha256") != sha256_file(mask_path):
        print(
            f"warning: {mask_path} does not match template.json; rebuild it with --mask-from",
            file=sys.stderr,
        )
        return
    template_json["members"]["mask"] = member
    template_json["files"]["brainmask.nii.gz"] = entry
    template_json["mask_method"] = method


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--zip", type=Path, help="mni_icbm152_nlin_asym_09b_nifti.zip")
    ap.add_argument(
        "--mask-from",
        type=Path,
        help="mni_icbm152_nlin_asym_09c_nifti.zip: rebuild only brainmask.nii.gz",
    )
    ap.add_argument(
        "--bridge-dir",
        type=Path,
        help="cached MNI152NLin2009cAsym bridge directory (default: the one in the per-user data folder)",
    )
    ap.add_argument(
        "--out",
        type=Path,
        default=Path(__file__).resolve().parents[1]
        / "resources/templates/space"
        / SPACE
        / "res-1mm",
    )
    args = ap.parse_args()

    if args.mask_from is not None:
        template_path = args.out / "template.json"
        if not template_path.is_file():
            print(f"error: {template_path} not found", file=sys.stderr)
            return 1
        template_json = json.loads(template_path.read_text(encoding="utf-8"))
        member, entry, method = build_mask(
            args.mask_from,
            args.bridge_dir or default_bridge_dir(),
            args.out,
            Path(__file__).resolve().parents[1],
        )
        template_json["members"]["mask"] = member
        template_json["files"]["brainmask.nii.gz"] = entry
        template_json["mask_method"] = method
        template_path.write_text(
            json.dumps(template_json, indent=2) + "\n", encoding="utf-8"
        )
        return 0

    if args.zip is None:
        print("error: --zip is required unless --mask-from is given", file=sys.stderr)
        return 1

    zip_hash = sha256_file(args.zip)
    if zip_hash != ZIP_SHA256:
        print(
            f"error: {args.zip} sha256 {zip_hash} != pinned {ZIP_SHA256}",
            file=sys.stderr,
        )
        return 1

    args.out.mkdir(parents=True, exist_ok=True)
    meta_members: dict[str, dict[str, str]] = {}
    written: dict[str, dict[str, str]] = {}
    with zipfile.ZipFile(args.zip) as zf:
        for key, member in MEMBERS.items():
            raw = zf.read(member)
            meta_members[key] = {"member": member, "sha256": sha256_bytes(raw)}
            payload, shape = decimate(raw)
            target = args.out / f"{key}.nii.gz"
            write_gz(target, payload)
            written[target.name] = {"sha256": sha256_file(target), "shape": list(shape)}
            print(f"{target}: shape {shape}, {target.stat().st_size} bytes")
        licence = zf.read(LICENCE_MEMBER)
        meta_members["licence"] = {
            "member": LICENCE_MEMBER,
            "sha256": sha256_bytes(licence),
        }
        licence_path = args.out / "LICENSE"
        licence_path.write_bytes(licence)
        written["LICENSE"] = {"sha256": sha256_file(licence_path)}

    template_json = {
        "space": SPACE,
        "res_mm": 1.0,
        "entry_id": ENTRY_ID,
        "source_url": SOURCE_URL,
        "zip_sha256": ZIP_SHA256,
        "members": meta_members,
        "files": written,
        "method": METHOD,
    }
    carry_over_mask(args.out, template_json)
    (args.out / "template.json").write_text(
        json.dumps(template_json, indent=2) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
