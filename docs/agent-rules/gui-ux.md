# egui UX rules (Wyss DBS Localizer)

**Applies to:** `crates/dbsloc-viz/**`, `crates/dbsloc-app/**`

## State persistence

All user-adjustable state (`SceneShared`, camera, slice indices, atlas visibility, dock layout) must implement `serde::Serialize + Deserialize` and be saved via `eframe`'s built-in persistence (`eframe::App::save`). Users must not lose their camera position or atlas selections on restart.

## Keyboard shortcuts

Every interactive control must have at least a tooltip. Frequently used actions require a keyboard shortcut:

| Action | Shortcut |
|--------|----------|
| Reset view | `R` |
| Toggle axial slice | `1` |
| Toggle coronal slice | `2` |
| Toggle sagittal slice | `3` |
| Save scene screenshot | `Ctrl+S` |
| Next/prev atlas structure | `↑` / `↓` |
| Pop out the focused 3D Viewer or Registration review tab | `F11` |
| Focus the Registration review tab | `Ctrl+5` |
| Open the *Import images* wizard (`specs/01` §8.4) | `Ctrl+I` |
| Focus the *Refine localization* tab | `Ctrl+6` |
| Open *Group analysis* (`specs/14` §5) | `Ctrl+7` |
| Show the *Help* window (shortcuts, system information) | `F1` |
| Scan the input directory again (also runs when the window regains focus) | `F5` |
| Stop the run (or a running save of lead corrections); with subjects still queued in a batch it asks *Stop after this subject* / *Stop now* / *Keep running* | `Ctrl+.` |

Registration review (`dbsloc_viz::checkreg_app::shortcut_rows()`, only while the review is shown and no text field has focus; `1` / `2` / `3` mean single pane there, not slice toggles):

| Action | Shortcut |
|--------|----------|
| Approve image and go to the next | `A` |
| Reject image and go to the next | `D` |
| Previous / next image | `←` / `→` |
| Previous / next slice | `PageUp` / `PageDown` |
| Single pane axial / coronal / sagittal | `1` / `2` / `3` |
| 2×2 layout | `0` |
| Reset zoom and pan | `R` |

Refine localization (`dbsloc_viz::refine_app::shortcut_rows()`, `specs/08` §11.1.6; only while the view is shown and no text field has focus):

| Action | Shortcut |
|--------|----------|
| Select the head (contact 0) / tail marker | `H` / `T` |
| Previous / next lead side | `<` / `>` |
| Nudge the selected marker in the hovered pane (0.25 mm; Shift 1 mm; Ctrl 0.05 mm) | `←` `→` `↑` `↓` |
| Move the selected marker through the pane plane | `PageUp` / `PageDown` |
| Move the whole lead in the hovered pane | `Alt` + arrows |
| Move the whole lead along its axis, proximal / distal (0.25 mm; Shift 1 mm; Ctrl 0.05 mm) | `Alt+PageUp` / `Alt+PageDown` |
| Keep lead length on / off | `K` |
| X-ray (slab) on / off (lead panes) | `X` |
| Hide / show markers in every pane | `V` |
| Lead planes view | `L` |
| Roll −1° / +1° (Shift: 10°) | `[` / `]` |
| Accept this side and go to the next | `Space` |
| Undo / redo for this side | `Ctrl+Z` / `Ctrl+Y` |
| Save corrections | `Ctrl+Enter` |
| Discard and close | `Esc` |
| Centre the crosshair on the selected marker | `F` |
| Single pane axial / coronal / sagittal, the three voxel-aligned planes | `1` / `2` / `3`, `0` |
| Reset zoom and pan | `R` |

Image preview (`dbsloc_viz::volume_preview::shortcut_rows()`, `specs/08` addendum §6.7; only while the pointer is over the preview and no text field has focus):

| Action | Shortcut |
|--------|----------|
| Single pane axial / coronal / sagittal | `1` / `2` / `3` |
| 2×2 layout | `0` |
| Previous / next slice in the single pane | `PageUp` / `PageDown` |
| Reset zoom, pan and window | `R` |

Import session report window (`specs/16` §8.2; only while the window is open):

| Action | Keys |
| ------ | ---- |
| Cancel / stop after the current file / close | `Esc` |
| Import (when enabled and no text field has focus) | `Enter` |

Register shortcuts with `ui.input_mut(|i| i.consume_shortcut(...))`.

## Loading progress

Loading states must show `loaded / total` counts, not just a spinner. Use `egui::ProgressBar` with a fraction. The label must be the item name being loaded, not generic "Loading…".

## Layout

Use `egui_dock` for dockable panels in complex views. Do not use hard-coded pixel widths or heights; derive sizes from `ui.available_size()` and `egui::FontId` metrics.

## Accessibility

Use `egui::Response::on_hover_text` on all sliders and buttons. Use descriptive widget names (not "slider_1") for AccessKit screen reader support. `eframe` must enable the `accesskit` feature.

## User-facing labels

- "Directory" not "dir"
- "Opacity" not "α" or "alpha"
- "Distance (mm)" with unit when applicable
- Spell out "Window / Level" in dedicated help text

## Colors

Atlas structure colors must be user-overridable and persisted. Never rely solely on a hash-derived color with no override path.

## Brand

Every window follows the Wyss Center brand as laid out in `docs/BRAND.md`: IBM Plex Sans (installed once at creation by `dbsloc_viz::brand::install_fonts`), the brand greys, and **one accent hue** (the brand green, muted) from `dbsloc_viz::ui_tokens`. Status colours stay muted and colour-blind safe. Never use the raw brand green or pink as large fills — or hard-code any hex value in a panel; take colours from `ui_tokens::palette_for(ui)` and keep the contrast tests in `ui_tokens.rs` passing when a token changes.

## Start screen and guided run

Words are a cost: a screen that needs a paragraph to explain itself is the wrong screen. The start screen and each guided step use buttons, list rows and a 2-4 word label; the explanation lives in a hover tooltip, never in body text.

- The application opens on the start screen. Recent subjects, recent folders, *Open folder…* and *Open subject…* are the whole of it; one click on a recent subject opens it.
- The guided run shows one step at a time (Subject, Preprocess, Coregister, Normalize, Reconstruct, Review). A step reuses the ordinary step card and the shared option editors; it never copies them. A step is judged as a run of that step alone, so an unselected upstream step is not counted as available.
- Back / Run / Next sit in a pinned bar. Next is enabled once the step ran or has output; when the step cannot run at all, Next reads *Skip* so the guide is never a dead end. *Switch to full view* and *Home* (Ctrl+H) are always reachable.
- Glyphs must exist in the installed brand font; check a screenshot, because a missing glyph renders as an empty box.

## Error display

Errors must appear as an `egui::Window` with the full message and a "Copy" button, not only as `eprintln!` to stderr.
