# Brand — Wyss Center look for the Wyss DBS Localizer windows

**Applies to:** `dbs-localizer` including its `scene` and `checkreg` modes (everything built on `crates/dbsloc-viz` `interactive-eframe`).
**Code:** `crates/dbsloc-viz/src/brand.rs` (typeface, avatar, window icon), `crates/dbsloc-viz/src/ui_tokens.rs` (colour tokens + contrast tests).

## Rule: keep colour minimal

Wyss DBS Localizer is a clinical tool. The windows are neutral grey and black from the brand sheet with **one accent hue** — the brand green, always in a muted / darkened form. Status colours (ok / warn / error / info) stay semantically distinct and colour-blind safe, but muted. Never use the raw brand green `#0DE69F`, the lighter green `#95F9D8`, pink `#E541F6` / `#EF8AF9` or lime `#DFFE80` as large fills or as UI colours at all; the raw green may only appear in marketing material, not in the application.

Every colour a window draws goes through `ui_tokens::palette(dark)`; nothing hard-codes a hex value in a panel. Foreground tokens (`ok`, `warn`, `err`, `info`, `accent`) and `*_fill` tokens (white text on them) are not interchangeable — the contrast tests in `ui_tokens.rs` assert WCAG AA for each family in both themes.

## Typeface

IBM Plex Sans (SIL Open Font License 1.1, © 2017 IBM Corp.; see `NOTICE`).

| Use | Family | File |
| --- | --- | --- |
| Body, buttons, small text | `FontFamily::Proportional` → `IBMPlexSans` | `assets/fonts/IBMPlexSans-Regular.ttf` |
| Headings (`TextStyle::Heading`) | `FontFamily::Name("IBMPlexSans-Medium")` | `assets/fonts/IBMPlexSans-Medium.ttf` |
| Strong emphasis where a heavier weight is needed | `FontFamily::Name("IBMPlexSans-SemiBold")` | `assets/fonts/IBMPlexSans-SemiBold.ttf` |
| Monospace (logs, paths) | egui default (Hack) — the brand specifies no monospace face | egui |

`brand::install_fonts(ctx)` puts Plex first in the proportional chain and keeps egui's own faces behind it as glyph fallbacks (✔ ✖ ⓘ → and emoji), so no symbol the UI uses goes missing. Call it once at creation, before styles; the application does it in `DbslocApp::new`, the standalone viewers in their `run_native` closures (`brand::apply_theme` additionally paints the token palette on both themes and maps the heading style to Medium).

## Token → brand colour mapping

Official values are from the Wyss Center palette sheet; "derived" values are tuned from the nearest official colour until every contrast test in `ui_tokens.rs` passes (4.5:1 for text on its surface, 3:1 for the focus ring, 4.5:1 for white text on fills).

| Token | Light | Dark | Source |
| --- | --- | --- | --- |
| `canvas` | `#E6E6E6` | `#0B0B0B` | Grey light (official) / derived from Black `#000000` |
| `surface` | `#F8F8F8` | `#161616` | Grey lighter (official) / derived |
| `surface_raised` | `#FFFFFF` | `#202020` | derived (white above Grey lighter) / derived |
| `hairline` | `#D8D8D8` | `#2E2E2E` | Grey (official) / derived |
| `hairline_strong` | `#949494` | `#4A4A4A` | Grey mid (official) / derived |
| `text` | `#111111` | `#F8F8F8` | near Black (derived from `#000000`, see below) / Grey lighter (official) |
| `text_muted` | `#666666` | `#949494` | derived from Grey dark `#6F6F6F` (see below) / Grey mid (official) |
| `accent`, `ok` | `#0A7050` | `#27B386` | muted Green `#0DE69F` |
| `accent_fill`, `ok_fill` | `#0A7050` | `#0F6B4C` | muted Green `#0DE69F` |
| `warn` | `#8A5300` | `#D9A04A` | amber, derived toward Red light `#FFBB99` |
| `warn_fill` | `#8A5300` | `#7A4D0A` | same family |
| `err` | `#A83605` | `#FF8367` | vermillion, derived toward Red light `#FFBB99` (the only red) |
| `err_fill` | `#A83605` | `#7A2F1E` | same family |
| `info` | `#36659B` | `#77B3F8` | derived from Blue light / Blue light (official) |

Derivations worth knowing:

- Light `text` is `#111111`, not the official `#000000`: both are far past AA (17.8:1 vs 19.8:1 on `surface`); the near-black avoids the hard halation of pure black on bright LCDs.
- Light `text_muted` is `#666666` because the official Grey dark `#6F6F6F` measures 4.0:1 on Grey light (`canvas`), under the 4.5:1 the `text_muted` test asks for.
- Light `accent` is `#0A7050` (not `#0B7A57`) and `info` is `#36659B` (not `#3D6FA8`) for the same reason: both must stay ≥ 4.5:1 on the `canvas`, not only on `surface`.
- The keyboard focus ring is `accent`, 2 px, expanded; the selection highlight is `accent_fill` at 55 %.

## Assets

| Asset | Path | Use |
| --- | --- | --- |
| Plex fonts + OFL | `crates/dbsloc-viz/assets/fonts/` | `include_bytes!` in `brand.rs` (`interactive-eframe` only, so headless builds carry none of it) |
| Avatar, white glyph on transparent | `crates/dbsloc-viz/assets/brand/wyss-center-icon-inverse-256.png` | top-bar logo (`egui::Image::tint` with the theme's `text` colour) and the landing-screen mark |
| Avatar, black glyph on transparent | `crates/dbsloc-viz/assets/brand/wyss-center-icon-256.png` | reference master for print / docs; not compiled in |
| Application tile (draft, 2026-10-15) | `crates/dbsloc-viz/assets/brand/dbs-loc-icon-draft.png` (square master supplied by the designer, a Wyss Center employee; rights with the Wyss Center, GPL-3.0-only) → `dbs-loc-icon-1024.png` and `dbs-loc-icon-256.png` | OS window / task-bar icon (`brand::window_icon`): black circuit-and-brain motif with the **LOC** wordmark on a mint rounded square, transparent corners. The draft is a placeholder until the final artwork arrives |
| Executable icon (Windows) | `crates/dbsloc-viz/assets/brand/dbs-loc-icon.ico` (16, 24, 32, 48, 64, 128, 256 px from the 1024 px master) | Embedded in `dbs-localizer.exe` by `crates/dbsloc-app/build.rs` (resource `1 ICON`, compiled with the Windows SDK `rc.exe` or `windres`; a missing tool is a build warning, not an error), so Explorer, archives and shortcuts show the tile |

### Regenerating the 256 px icons from the master

From the Wyss Center vector avatar (SVG or high-resolution PNG, glyph on a transparent background):

```bash
# Black glyph (master), 256 px, transparent background
magick avatar.svg -background none -resize 256x256 -gravity center -extent 256x256 \
  -alpha on wyss-center-icon-256.png
# White glyph: keep the alpha mask, paint every opaque pixel white
magick wyss-center-icon-256.png -fill white -colorize 100% wyss-center-icon-inverse-256.png
```

Both files must stay 256 × 256 RGBA (8 bit) with a straight-alpha mask: `brand::tests::the_avatar_decodes_as_a_256_px_alpha_mask` checks the size and that every opaque pixel of the inverse file is pure white.

### Regenerating the application icon files

All three icon files come from one square RGBA master (transparent outside the tile):

```bash
uv run --with pillow --no-project python -I scripts/build_app_icon.py [MASTER.png]
```

Without an argument the script reads `dbs-loc-icon-draft.png`; it writes `dbs-loc-icon-1024.png`, `dbs-loc-icon-256.png` and `dbs-loc-icon.ico` beside it (Pillow is not a project dependency). For the final artwork, pass its path (or replace the draft file) and rebuild `dbs-localizer`; `build.rs` re-embeds the `.ico`. `brand::tests` checks that the window icon is 256 × 256, has a transparent corner, and contains both dark and light opaque pixels.

The earlier "DBS-LOC" tile (white Wyss avatar over a black rounded square, Plex SemiBold wordmark) is retired; it is in git history before 2026-10-15.
