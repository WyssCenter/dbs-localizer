# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

import sys
from pathlib import Path

from PIL import Image

BRAND = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "dbsloc-viz"
    / "assets"
    / "brand"
)
ICO_SIZES = [(s, s) for s in (16, 24, 32, 48, 64, 128, 256)]


def main() -> None:
    source = (
        Path(sys.argv[1]) if len(sys.argv) > 1 else BRAND / "dbs-loc-icon-draft.png"
    )
    image = Image.open(source).convert("RGBA")
    if image.width != image.height:
        raise SystemExit(
            f"{source}: icon must be square, got {image.width}x{image.height}"
        )
    master = image.resize((1024, 1024), Image.LANCZOS)
    master.save(BRAND / "dbs-loc-icon-1024.png", optimize=True)
    master.resize((256, 256), Image.LANCZOS).save(
        BRAND / "dbs-loc-icon-256.png", optimize=True
    )
    master.save(BRAND / "dbs-loc-icon.ico", format="ICO", sizes=ICO_SIZES)
    print(f"icon files written to {BRAND} from {source}")


if __name__ == "__main__":
    main()
