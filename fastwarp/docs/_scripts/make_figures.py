# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Make the registration figure of the documentation from public data.

Downloads one subject of the SNSX dataset (OpenNeuro ds004470 1.0.1, CC0) and the MNI ICBM 2009c
template from TemplateFlow, checks both against the SHA-256 pinned for the accuracy benchmark,
registers the subject onto the template with :func:`fastwarp.register_syn`, and writes
``docs/_static/figures/registration-checkerboard.png``.

Run from the repository root with ANTsPy, matplotlib and a GPU::

    uv run --no-sync python docs/_scripts/make_figures.py --cache ~/fastwarp-figures
"""

from __future__ import annotations

import argparse
import hashlib
import json
import urllib.request
from pathlib import Path

import ants
import fastwarp
import numpy as np
from matplotlib.figure import Figure

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / "docs" / "_static" / "figures" / "registration-checkerboard.png"
FILES = {
    "template": ("mni2009c", "tpl-MNI152NLin2009cAsym_res-01_T1w.nii.gz"),
    "subject": ("snsx", "sub-C001/anat/sub-C001_acq-MP2RAGE_run-01_T1w.nii.gz"),
}


def fetch(cache: Path, dataset: str, path: str) -> Path:
    pins = json.loads((REPO / "benchmarks" / "accuracy" / "pins.json").read_text())
    pin = next(p for p in pins[dataset] if p["path"] == path)
    target = cache / Path(path).name
    if not target.is_file():
        if not pin["url"].startswith("https://"):
            raise SystemExit(f"refusing a non-HTTPS download: {pin['url']}")
        with urllib.request.urlopen(pin["url"], timeout=120) as response:
            if not response.geturl().startswith("https://"):
                raise SystemExit(f"refusing a redirect away from HTTPS: {response.geturl()}")
            target.with_suffix(".part").write_bytes(response.read())
        target.with_suffix(".part").replace(target)
    data = target.read_bytes()
    if len(data) != pin["size"] or hashlib.sha256(data).hexdigest() != pin["sha256"]:
        target.unlink()
        raise SystemExit(f"{target.name} does not match its pinned size and SHA-256")
    return target


def normalized(image: ants.ANTsImage) -> np.ndarray:
    data = image.numpy()
    return np.clip(data / np.percentile(data[data > 0], 99.5), 0.0, 1.0)


def axial(volume: np.ndarray, k: int) -> np.ndarray:
    return np.rot90(volume[::-1, :, k])


def checkerboard(a: np.ndarray, b: np.ndarray, tiles: int = 10) -> np.ndarray:
    rows = np.arange(a.shape[0]) * tiles // a.shape[0]
    cols = np.arange(a.shape[1]) * tiles // a.shape[1]
    mask = (rows[:, None] + cols[None, :]) % 2 == 0
    return np.where(mask, a, b)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cache", type=Path, required=True, help="folder for the downloads")
    args = parser.parse_args()
    cache = args.cache.expanduser()
    cache.mkdir(parents=True, exist_ok=True)

    fixed = ants.image_read(str(fetch(cache, *FILES["template"])))
    moving = ants.image_read(str(fetch(cache, *FILES["subject"])))
    info = fastwarp.register_syn(fixed, moving, f"{cache.as_posix()}/reg_")
    print(f"{info['adapter']}: {info['total_s']:.1f} s on a {info['grid']} grid")

    before = ants.resample_image_to_target(moving, fixed)
    after = fastwarp.apply_transforms(moving, fixed, [cache / "reg_Composite.h5"])
    before, after = (ants.histogram_match_image(img, fixed) for img in (before, after))
    k = fixed.shape[2] // 2 + 8
    template, start, end = (axial(normalized(img), k) for img in (fixed, before, after))

    panels = [template, checkerboard(template, start), checkerboard(template, end)]
    fig = Figure(figsize=(9.0, 3.6), dpi=110, facecolor="black")
    axes = fig.subplots(1, 3)
    for ax, panel in zip(axes, panels, strict=True):
        ax.imshow(panel, cmap="gray", vmin=0.0, vmax=1.0, interpolation="lanczos")
        ax.set_axis_off()
    fig.subplots_adjust(left=0.005, right=0.995, top=0.99, bottom=0.01, wspace=0.02)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT, facecolor="black")
    print(f"wrote {OUT.relative_to(REPO)} ({OUT.stat().st_size // 1024} KiB)")


if __name__ == "__main__":
    main()
