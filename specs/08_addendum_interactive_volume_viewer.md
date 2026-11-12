# Addendum — Interactive volume viewer (NIfTI, desktop)

**Status:** Phase B addendum to [`08_visualization.md`](08_visualization.md)  
**Scope v1:** **Desktop only** (`dbsloc-viz` / `dbs-localizer checkreg`); **NIfTI** inputs (`.nii`, `.nii.gz`). **Out of scope v1:** DICOM P10 ingest, PACS/DICOMweb, browser-based viewer.  
**Integration:** Reuses checkreg **manifest JSON** (see [`03_coregistration.md`](03_coregistration.md) §12 and the pipeline gate `crates/dbsloc-pipeline/src/checkreg.rs`, §12.4a). **Normative for:** interactive QC of **coregistration** and **normalization** (and future brainshift-class pairs) when volumes share the same index grid as today’s `gencheckregpair` path.

---

## 1. Purpose

Deliver a **reusable** interactive 2D viewer so operators can verify registration and template alignment beyond a **single static montage PNG**: adjust **contrast/brightness** (window width / center on scalar samples), **zoom** and **pan**, and navigate **orthogonal planes** and **slice index**. Same binary and manifest contract should serve **coreg** and **norm** branches with only path/metadata differences.

**Non-diagnostic:** UI and docs state research / engineering QC only—not FDA-cleared primary diagnosis.

---

## 2. References (conceptual, no code import)

- **DICOM PS3.3** — Attributes **Window Center** (0028,1050) and **Window Width** (0028,1051) define clinical window/level semantics on **stored values**; v1 viewer applies the same **linear mapping** on NIfTI voxel values (may be HU, arbitrary float, or pre-scaled—**user adjusts** until tissue of interest is visible).
- **IHE Radiology** — *Basic Image Review* profile: expectations for pan, zoom, scroll, window/level in review workflows (feature categories only).
- **Clean-room** ([`AGENTS.md`](../AGENTS.md) §9): specification and public algorithms only—no copying from GPL applications (e.g. ITK-SNAP). Permissive stacks (e.g. MIT web viewers) are **not** v1 dependencies; optional **later** profile.

---

## 3. Open-source landscape (distribution vs reuse)

| Project | License (verify upstream) | Bundle whole app with Wyss DBS Localizer? | Reuse source in `dbsloc` (proprietary when this was written; GPL-3.0-only since 2026-11-10)? |
|--------|----------------------------|--------------------------------|--------------------------------------|
| **3D Slicer** | Custom permissive (SPDX `3D-Slicer-1.0`) | Theoretically possible; **very heavy** (Qt/VTK/Python stack) | Possible **fragments** with attribution + legal review—not default |
| **ITK-SNAP** | **GPL-3** | Separate user install OK | **Do not** embed or link into closed Wyss DBS Localizer binary without GPL compliance |
| **ITK** (library) | **Apache-2.0** | As optional library dep | **Yes**, with `NOTICE` |
| **OHIF / Cornerstone3D** | **MIT** | Optional **future** web profile | **Yes**, preserve copyright notices—**not v1** |
| **Wyss DBS Localizer `dbsloc-viz`** | Wyss (GPL-3.0-only since 2026-11-10) | Yes (in-tree) | Yes |

**Conclusion:** v1 extends **`dbs-localizer checkreg`** (`egui` / `wgpu` where used) and [`checkreg_pair`](../crates/dbsloc-checkreg/src/checkreg_pair.rs) CPU slice compositing—no GPL, no mandate to ship Slicer.

---

## 4. Inputs and spaces

| Field | Required | Description |
|-------|----------|-------------|
| `moving` | Yes | Path to registered **moving** NIfTI (same grid as fixed for v1). |
| `fixed` | Yes | Path to **fixed** (anchor or template) NIfTI. |
| `checkreg_png` | Yes | Batch montage path (regenerate on Refresh; fallback if volume load fails). |
| `branch` / `modality` / `method_log` | As today | QC branch, modality label, JSON log for approval. |
| Affine | From NIfTI header | Used for future mm display; v1 may show **index** + optional voxel size readout. |

**Grid:** Same as §12.2 visual contract: moving and fixed **same dimensions** along `dim0×dim1×dim2` (i,j,k index ordering as in existing Rust loader). Mis-sized pair → error string in UI (existing behavior).

---

## 5. Manifest schema

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `manifest_version` | `u32` | `1` | Forward compatibility for new fields. |
| `subj_id` | string | — | Window title and labels. |
| `active` | `usize` | `0` | Active slide index. |
| `slides` | array | — | Slide objects (see pipeline). |
| `subj_dir` | path optional | — | “Open patient dir”. |

Implementations **must** accept manifests **without** `manifest_version` (treat as `1`).

---

## 6. Functional requirements (v1)

### 6.1 Planes and navigation

- **Planes:** axial, coronal, sagittal—consistent with [`CheckregSlicePlane`](../crates/dbsloc-checkreg/src/checkreg_pair.rs).
- **Slice index:** integer slider or equivalent; clamped to volume extent.
- **Optional (later):** keyboard shortcuts (documented in UI tooltip).

### 6.2 Visual encoding (unchanged role from §12.2)

- **Greyscale:** displays **moving** intensity (after user-selected windowing—see §6.3—or legacy preprocessed 0–1 path for parity with batch PNG).
- **Overlay:** **red-highlighted edges** derived from **fixed** slice (e.g. Sobel magnitude threshold)—same family as batch PNG.

### 6.3 Window / level (interactive)

- **Parameters:** `center` C, `width` W (W > ε). Display mapping: `t = clamp((v - (C - W/2)) / W, 0, 1)` per **moving** sample `v` from **raw** NIfTI values **before** percentile preproc used for batch PNG.
- **UI:** sliders or drag inputs; **presets** (examples, not HU-calibrated unless data are HU):
  - **Soft tissue (CT-like):** e.g. C ≈ 40, W ≈ 80 (adjustable).
  - **Bone (CT-like):** e.g. C ≈ 300, W ≈ 1500 (adjustable).
  - **Reset** to preset defaults.
- **Mode toggle:** **“Match batch PNG”** (preprocessed moving) vs **“Raw + W/L”** so operators can compare with static montage.

### 6.4 Zoom and pan

- **Zoom:** discrete controls (e.g. ×0.25 … ×8) or scroll-wheel with modifier where slice scroll is primary—**must not** break slice navigation without documented modifier.
- **Pan:** when zoomed, scroll bars or drag-to-pan inside viewport.
- **Reset view:** restores zoom/pan defaults.

### 6.5 Batch PNG and Refresh

- **Refresh** regenerates montage from disk paths (existing behavior).
- If volume load fails, **fallback** to static PNG scroll (existing behavior).

### 6.6 Approval workflow

- Unchanged: write approval JSON to `method_log`; **Approve/Next**, **Disapprove/Next**, **Back**, last-slide labels per [`03_coregistration.md`](03_coregistration.md) §12.3.
- **Embedded (2026-10-14):** the viewer UI is the library panel `dbsloc_viz::checkreg_app::CheckregReviewPanel` (feature `checkreg-embed`); the `checkreg` mode of `dbs-localizer` (`dbs-localizer checkreg <options>`; the former `dbsloc_checkreg` binary was removed 2026-10-23, lookup `DBSLOC_GUI_BIN` → beside the executable → `PATH` → `target/{release,debug}`) is a thin window around it and the app's main window shows it in its *Registration review* tab. The panel returns decisions and never writes files: the host writes `method_log` (the `checkreg` mode with its own writer, the app with `dbsloc_pipeline::apply_approval`). Labels: *Approve & next* / *Reject & next*; last slide *Approve & continue run* / *Reject & stop run* during a run, *Approve & close* / *Reject & close* otherwise. Volumes load on a worker thread, one slide at a time. Keys (only without text focus): A approve, D reject, ← / → previous / next image, PageUp / PageDown slice, 1 / 2 / 3 single pane axial / coronal / sagittal, 0 the 2×2 layout, R reset zoom and pan. The *Recompute* button and the free-text *Method* field are removed (the method is recorded by the pipeline).
- **Slice caching (2026-11-12).** Each pane (the three panes of the 2×2 layout and the single pane) keeps three stages, each rebuilt only when its own inputs change: (1) the slice layers — the moving slice (raw or percentile-scaled moving image) and the edge mask of the fixed slice (Sobel magnitude, 10 % of the slice maximum) — keyed by plane, the pane's own slice index and the raw-window switch; (2) the coloured slice resampled to the physical aspect ratio, keyed by the layers plus window centre and width only while the raw window is on; (3) the uploaded texture, keyed by that image plus the crosshair position projected into the pane (or crosshair off). Hence a window / level change recolours and resamples each pane without reading the volumes or recomputing edges; a slice step rebuilds the layers of the pane whose index changed and only redraws the crosshair in the other two; a crosshair move redraws the crosshair line in a pane whose own index is unchanged; with the raw window off, window / level changes rebuild nothing. Each pane holds at most one layers set and one resampled image, replaced on change; all are dropped when a new pair starts loading and when it arrives. Output pixels are byte-identical to the one-pass `PreparedCheckregVols::rgba_slice_display` (same filter order, CPU only); `cargo test -p dbsloc-checkreg --features pair staged_slice_path` guards it.

### 6.7 Single-volume preview (2026-10-23)

A plain look at one NIfTI image, with no registration pairing: the embeddable panel `dbsloc_viz::volume_preview::VolumePreview` (feature `refine-embed`; texture and slice-cell helpers shared with the review panel in the private `slice_ui` module).

- **Where shown.** A floating, resizable, closable window *Image preview — <label>* owned by the application (`panels::ImagePreviewHost`; one preview at a time, opening another replaces it) and one owned by the Import images wizard. A *View* button opens it from every row of the wizard's Images step (`specs/01` §8.4), from the Pipeline panel's *Subject images* section (pre-op anchor in use, original, bias-corrected, other pre-op images, post-op CT, post-op MRI per orientation, coregistered outputs), from the Coregister card (the post-op images the *Post-op images to use* choice selects) and from the Normalize card (*Anchor*).
- **Loading.** The volume loads on a worker thread (`RefineVolume::load`, dimension guard included); the window names the file being loaded and shows a spinner, a failure shows the message with a *Copy message* button. The paint thread never reads the file.
- **Controls.** Header with file name, matrix size, voxel size in millimetres and intensity range. Layout toggle 2×2 (three planes + information cell) or one pane (*Axial* / *Coronal* / *Sagittal*). Per plane a slice slider (*Index*, with the slice's world position in millimetres) and shared *Window* / *Level* sliders with *Reset window*; the default window is `RefineVolume::default_window` and a CT hint from the modality (the importer's `CT` suffix, the pipeline's post-op CT) selects the bone window. Left click or drag in a pane moves the other two planes to that world point (crosshair). Wheel steps the slice; Ctrl+wheel or pinch zooms; right drag or Shift+left drag pans. Slices are re-rendered only when a plane's slice index or the window changes.
- **Keys** (only while the pointer is over the preview and no text field has focus): `1` / `2` / `3` single pane axial / coronal / sagittal, `0` the 2×2 layout, `Page Up` / `Page Down` previous / next slice in the single pane, `R` reset zoom, pan and window.
- **Acceptance.** It opens any NIfTI that the wizard or the pipeline lists; a CT is shown in the bone window; it never blocks the paint thread; the wizard's preview closes when its row, its input folder or its temporary conversion folder goes away, and when the wizard closes; every control has a hover text. Tests: `cargo test -p dbsloc-viz --features refine-embed,checkreg-embed volume_preview`, `cargo test -p dbsloc-app preview`.

---

## 7. Non-functional requirements

| Topic | Requirement |
|-------|-------------|
| **PHI** | No network I/O; local paths only. |
| **Performance** | Target **interactive** slice updates on typical DBS CT/T1w sizes on a developer GPU; document optional downscale if a future phase adds it. |
| **Headless / CI** | `--headless-check` validates manifest JSON only; no window. |
| **Testing** | Rust unit tests for slice compositing and window mapping sanity; CLI smoke for manifest (existing Python tests). |

---

## 8. Out of scope (explicit)

- DICOM file / network ingest; **C-FIND/C-MOVE**; DICOMweb.
- Web viewer (MIT OHIF profile)—**future addendum**.
- True **GPU raycast** MIP/MinIP/isosurface—**phase 2** in [`08_visualization.md`](08_visualization.md) 3D section.
- Cross-volume **resampling** when grids differ (viewer assumes aligned grids; resampling is pipeline’s job).

---

## 9. Acceptance criteria (v1)

1. Operator can switch planes and slices for a manifest with valid NIfTIs.
2. Operator can toggle **raw W/L** vs **batch-style** preprocessed moving display.
3. Operator can change zoom and reset view; pan works when image exceeds viewport.
4. Approval JSON behavior unchanged vs previous `dbsloc_checkreg` binary.
5. Omitted `manifest_version` still loads; `manifest_version: 1` round-trips from pipeline manifest writer.

---

## 10. Future: DICOM profile (placeholder)

- Ingest **DICOM series** → 3D volume; map **Window Center/Width** from first instance or user default.
- **Patient/study** metadata overlays (de-identified in research builds).

---

*This addendum is normative for interactive viewer behavior; [`08_visualization.md`](08_visualization.md) remains normative for batch D2/D3 pipeline outputs.*
