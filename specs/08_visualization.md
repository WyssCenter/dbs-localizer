# Spec 08 — Visualization (2D / 3D) (Wyss DBS Localizer)

**Status:** Phase B  
**Depends on:** `01_bids`, `04`, `05`, `07` for inputs  
**Phase A (archived):** the Phase A archive (removed 2026-10-23, in git history) — section `specs/phase_a_visualization_report.md`  
**Non-goal (v1):** pixel-perfect **MATLAB Elvis** clone — **file outputs** + **portable 3D** (obj/glTF optional).

**Interactive NIfTI QC viewer (coreg / norm):** [`08_addendum_interactive_volume_viewer.md`](08_addendum_interactive_volume_viewer.md) — desktop, window/level, zoom/pan, manifest contract.

---

## 1. Purpose

**Two channels:**

- **D2** — static **PNGs** (or SVG): orthogonal slices, electrode overlay, optional atlas, annotations.  
- **D3** — interactive 3D view **or** **off-screen** render (headless) for CI; may export `glb`/`obj`.

**Input:** `Subject`, `options.write_d2` / `write_d3`, `atlas_set` (atlas name or "segment patient" flow), `reco` path, `stim` VAT paths, colormap **prefs**.

**Out of scope:** “Sweet spot explorer” full UI — mark **optional** `phase_c` module.

---

## 2. Top-level `write` (orchestrator)

**Order:**

1. `determine_sides` → {L,R}.  
2. If `atlasset` == `segment_patient`: run subcortical **segmenter**; if not native template → `norm` segmentations.  
3. If `d2` → `write_planes`.  
4. If `d3` → `scene_build` (Elvis-like) + **optional** `export_server` + `close` if `auto_server`.  
5. `completion_haptic` (optional, no spec).

**Atlases** local path: strip `Local atlas: ` prefix; `Use none` → no atlas layers.

---

## 3. 2D — `write_planes`

**Params:** which planes (all / ax / cor / sag), **slice spacing** (mm) or N slices, `fig_visible`, **height** (mm) override for FOV, bound box from **contacts** (mean ± margin).

**Per slice:** reslice **MNI** (or **native** if `native` flag) anatomy; draw **contours** from **reconstruction** `coords` (per contact sphere or cylinder silhouette); **overlay** `ea_add_overlay` class for atlas labels (colormap, alpha).  
**Output:** one PNG per cut; naming `sub-…_…_acq-…_…_slice-###.png` (BIDS-sane pattern in impl).

**Zoom / crop:** fit **all contacts** in view with margin.

---

## 4. 3D — `scene` (elvis class)

**Layers:**

- **Anatomy** — isosurface or volume raycast from template **t1** (or post-op) with transfer function.  
- **Electrode** — mesh from `09` + `elstruct` (coords); materials in §6.  
- **Atlas** — isosurface per ROI (alpha, color).  
- **VAT** — isovolume (mesh from `isovalue` in options).  
- **Cortex** — if **FreeSurfer** path present (subprocess mesh load).

**Camera:** `zoom_center` = mean(contact positions), zoom ~3× (impl).

**Lighting:** 3-point **Blinn-Phong** or PBR; **params** in `visualization prefs`.

---

## 5. `roi` and atlas (data model)

- **Roi** = label id, NIfTI path, colormap, **opacity** [0,1], **hull** (mesh cache).  
- **Patient atlas** = same in **Subject.atlas** dir if segmented.

**Rendering:** for each ROI, **marching_cubes** at **0.5** (or isolevel) with **Laplacian** smooth **2** iters (reference defaults).

**Atlas in a non-reference template space** (`specs/04` §5.4a). The shipped AHEAD masks live in `MNI152NLin2009bAsym`. When the run's template space differs, `write_scene_manifest` obtains the bridge, resamples every mask NIfTI of the AHEAD directory (root and `lh` / `rh` / `mixed` / `midline`) onto the template grid with linear interpolation (marching cubes thresholds at 0.5), writes the mirrored layout under `<bridge dir>/atlases/ahead/structures/` (staged in `.partial-structures`, renamed; reused while every mask is present) and points `atlas_lead_sets` there; the manifest `space` is the template space id. If the bridge fails or is disabled (`DBSLOC_NO_BRIDGE=1`) the manifest has no default atlas and the log says `dbsloc: warning: scene: no atlas for template space <s>: <reason>`. `DBSLOC_SCENE_ATLAS_LEAD_DIR` keeps overriding the default set as given (the user vouches for its space).

---

## 6. Materials (electrode)

| Part | `rgba` (example) | Notes |
|------|------------------|--------|
| **Contact** | 0.3 grey | Metal |
| **Insulator** | 0.7 | Shaft |
| **Tip** | 0.3 or 0.7 | if `tip_is_contact` |

**VAT iso:** **yellow** 1.0 / **alpha** 0.35 in reference — implementation may theme.

---

## 7. Output file manifest

- **D2:** list of `png` + **json** index optional.  
- **D3:** **no** file if interactive only; if **export** → `scene.glb` or screenshot **png**.

**Logs:** `methods.txt` string for **publication** (algorithm narrative).

---

## 8. Edge cases

- **No recon** file → `E_VIZ_recon`.  
- **Atlas** missing file → **skip** layer + warning.  
- **4D** BOLD: take **Tmean** in writer pre-step.

---

## 9. Acceptance criteria

- [ ] **Golden** 3-contact reco + **MNI t1** → **PNG** dimensions non-zero, SHA stable on fixed backend.  
- [ ] 3D: **one** off-screen **png** in **headless** CI.  
- [ ] **Atlases** off → no crash, smaller manifest.

---

## 10. References

- **Lead-DBS** visualization figure conventions (paper supplement).  
- NIfTI orientation RAS for overlay alignment.

---

## Addendum A — Implementation architecture (locked)

**Goal:** one technical story so D2/D3 work does not splinter across ad hoc Python stacks.

| Layer | Role |
|-------|------|
| **Python (`dbsloc.visualization`)** | Orchestration only: BIDS paths, options, when to run, output manifest, subprocess/FFI to Rust. **No** production hot loops over voxels in pure Python. |
| **Rust (`crates/dbsloc-viz`)** | Default home for **2D/3D** work: reslice-at-scale, marching cubes, mesh I/O, **GPU** draw/compute, PNG / `glb` / off-screen export. |
| **Rust (`fastwarp/crates/fastwarp-gpu`)** | Shared wgpu device layer (kernels, device buffers) of the registration engine (Apache-2.0); the VAT / FEM solver builds on it in `crates/dbsloc-vat-gpu`. |

**GPU backends (normative order):**

1. **Portable (default):** **WebGPU via `wgpu`** in Rust — interactive window **or** headless off-screen (CI), Linux/macOS/Windows where drivers exist.
2. **NVIDIA fast path (optional):** **CUDA** via `cudarc` — when an NVIDIA device is available; must produce the **same artifact types** (e.g. PNG / `glb`) as the `wgpu` path, not a second incompatible format family.
3. **Fallback:** CPU Rust when no usable GPU.

**Explicit non-choice for *core* repo:** a heavy **Python** visualization engine (e.g. VTK-first stack) as the **primary** renderer — see `AGENTS.md` dependency policy. Thin Python calling **Rust** stays the rule.

**Bootstrap exception:** the pipeline `visualize` stage (Rust) writes the 3D scene manifest; the former Matplotlib electrode QC montage after `visualize` is **retired 2026-10-12** (use the checkreg viewer and the 3D scene). The former mid-slice `preview` helper is removed. Production D2/D3 should move to **`dbsloc-viz`** as APIs land.

**Interactive 3D scene — manifest v1 (implemented):** JSON consumed by `dbs-localizer scene` (the viewer mode of the GUI program; library `dbsloc-viz`, feature `scene-viewer`; the separate `dbsloc_scene3d` binary was removed 2026-10-23; resolution `DBSLOC_GUI_BIN` → beside the executable → `PATH` → `target/{release,debug}`). Fields: `manifest_version` (=1), `subj_id`, `space` (e.g. MNI152NLin2009bAsym), `template_nii` (NIfTI path, same mm space as reco MNI coords), `reco_json` (Reco v2), optional `atlas_obj_dir` (directory of `.obj` meshes), optional `atlas_nii` + `atlas_max_labels` (integer label volume → marching cubes). CLI: `dbsloc viz scene --manifest PATH`; pipeline: after successful reconstruct, when `normalization_template` exists as a file, **`viz/scene_manifest.json`** is written; **`dbs-localizer scene` opens by default** when a display is available (`RunOptions.open_scene3d_after_reconstruct` / `scene3d_interactive`; `dbsloc run --no-scene3d`, `DBSLOC_OPEN_SCENE3D=0` disables launch only; `--headless` / `DBSLOC_HEADLESS=1` skips GUI). Atlas redistribution: default redistributable AHEAD masks are documented in `docs/ATLAS_AHEAD.md`; DISTAL Minimal is bundled with the Lead-DBS licence statement beside it; other Lead-DBS atlases come from a Lead-DBS directory or are downloaded by the user (`docs/ATLAS_DISTAL.md`, `specs/11` §10).

**3D scene rendering (normative for the current scene viewer (`dbs-localizer scene`), 2026-10-16):** the scene renders off-screen in the paint callback's prepare step (`dbsloc_viz::SceneWgpuRenderer`, `scene_oit.rs`) into its own targets at the pixel size of the scene rectangle (egui's rounded and screen-clamped viewport, reallocated only when that size changes): an opaque colour target in the host surface format, cleared to transparent, with its own **24-bit depth buffer** (`Depth24Plus`), and two order-independent-transparency targets. One full-screen triangle in egui's render pass then composites the result over whatever egui drew beneath the scene rectangle (premultiplied-alpha blending; the composite neither tests nor writes egui's depth attachment, which `SCENE_VIEWER_DEPTH_BUFFER_BITS` still declares). The off-screen passes are single-sampled, like the host pass, and the off-screen work is skipped when the payload, the scene revision, the target size and the aspect ratio are unchanged since the last frame. **Template slice 2D textures:** axial layout matches `mni_slice` (width = dim `j`, height = dim `i`); coronal and sagittal use **superior-up** row order (high head on texture top after reversing `k` when rasterizing) with UVs tied to the same world quad corners. **Draw order:** opaque pass — electrode bodies (opaque, depth write, `Less`), then the reconstruction polyline when one is drawn, then the slice quads; atlas pass — the atlas meshes; then the composite. **Template slice quads:** alpha blend, `Less`; depth **write** only for a plane at full opacity, a translucent plane tests but does not write, so the atlas and the leads stay visible through it. **Atlas meshes (weighted blended order-independent transparency, McGuire & Bavoil 2013):** every visible atlas mesh is drawn in one indexed draw in upload order, without any sort, with depth **test** `LessEqual` against the opaque depth and no depth write, both faces (no culling), into an accumulation target (`Rgba16Float`, additive: premultiplied colour · *w* in rgb, α · *w* in a) and a revealage target (`R16Float`, cleared to 1, multiplied by 1 − α per fragment). The weight is *w* = α · clamp(8 (1 − *u*)³, 0.01, 8), with *u* = clamp(0.5 + 0.5 · *d* / *r*, 0, 1), where *d* is the distance of the fragment along the camera's forward direction from the centre of the axis-aligned bounding box of the visible atlas vertices and *r* is half that box's diagonal (at least 1 mm), so nearer fragments weigh more and the weight does not depend on the camera distance. The composite writes colour = (Σ premultiplied colour · *w* / Σ α · *w*) · (1 − *R*) + opaque colour · *R* and alpha = (1 − *R*) + opaque alpha · *R*, with *R* the revealage. The result does not depend on the order of the structures or of their triangles, so folded structures and nested or touching structures (GPi inside GPe, STN next to SN) no longer flicker or swap while the camera turns, and a camera move needs no CPU work on the atlas. A single translucent layer composites exactly as ordinary alpha blending (colour · α + background · (1 − α)); overlapping layers are a weighted average rather than a strict front-to-back *over*. A lead behind a structure is tinted by it and a lead in front of it stays opaque; a translucent slice does not write depth, so atlas surfaces behind it still composite over it; **per-structure α** in UI multiplies a global atlas opacity; **Reset view** restores initial camera target, orbit angles, distance, and slice indices from manifest load. **Electrode body placement (2026-10-15, reference-implementation behaviour):** the catalogue body (`09`; axis +z, head fiducial at `z_h` = centre of the most distal level, tail fiducial at `z_t` of `09` §2.2 (centre of the 4th level, or of the last level when the model has fewer than 4), lead radius `r`) is mapped by the one affine that sends the model fiducials head, tail, head + r·e_x, head + r·e_y onto the side's markers `head`, `tail`, `x`, `y`. Because `x` / `y` lie at radius `r` perpendicular to `û = (tail − head)/|tail − head|` (`06` §10.6), this is `T(p) = head + [x̂ ŷ û] · diag(1, 1, s) · (p − head_model)` with `s = |tail − head| / (z_t − z_h)`: rotation + translation + a stretch along the lead only. Every vertex (tip, contact bands, gaps, shaft) goes through `T`, so all axial lengths scale by `s` while the radius and the segment angles stay physical; the body is never bent along the trajectory and never fitted to `coords_mm`. In native space `s ≈ 1`; in template space `s` follows the warped head and tail (Lead-Tutor cohort: 1.07–1.31), and the contact bands land on the template-space contacts. A zero catalogue span (single level) uses `s = 1`. The shaft stays straight along `û` and ends at the axial projection of the most proximal trajectory sample (unchanged rule; that sample is already in the same space as the markers, so it is not scaled again). **Reco trajectory polyline (fallback only, 2026-10-15):** drawn only for a side without an electrode body (electrode model not in the `09` catalogue, or no markers), so a lead is never invisible; never drawn next to a body (the reference implementation draws no reconstruction line in its 3D view), because the 50-point line starts 5 mm below the tip (`05` §5) and, in files written before 2026-11-14, its template-space points were warped one by one, so it can bend away from the straight body. No user toggle; a saved `show_trajectories` from an older version is ignored. Opaque, depth **test** and depth **write** (2026-10-16, drawn in the opaque pass before the slices, so the atlas tints it like a lead body). **Atlas layers:** meshes from `atlas_obj_dir` / tessellated `atlas_nii` only — not voxel-raycast “volume” of a label map without meshing.

**Review:** change this addendum only with arch agreement (same bar as `AGENTS.md` stack decisions).

---

## 11. Addendum — Lead-DBS GUI reference (D2 / D3 and satellites)

**Status:** Behavioral reference for Wyss DBS Localizer visualization parity; **not** a GPL code import.  
**Source tree:** Local Lead-DBS (e.g. `ea_elvis.m`, `ea_writeplanes.m`, `ea_anatomycontrol.m`, `ea_atlasselect.m`, …).

**Doc map:** `AGENTS.md` § Documentation map lists where checkreg (`specs/03_coregistration.md` §12) vs this §11 live.

### 11.0 Scope

| Area | Lead-DBS entry | Wyss DBS Localizer v1 target |
|------|----------------|------------------|
| D3 “Electrode-Scene” | `ea_elvis` | `scene_build` + future `dbsloc-viz` / egui |
| D2 slice export + preview | `ea_writeplanes` | `write_planes` + PNG; optional figure preview |
| Orthogonal slice browser | `ea_imshow3d` (standalone) | optional |
| NIfTI pair / blend viewer | `ea_imshowpair` | shared with `specs/03_coregistration.md` §12.4 |
| Anatomy slice companion | `ea_anatomycontrol` + `ea_anatomyslices` | toggle cut planes on `resultfig` |
| Atlas picker | `ea_atlasselect` | atlas layer UI |
| Trajectory / planning | `ea_trajectorycontrol` | out of core Wyss DBS Localizer (defer) |
| Group / Connectome / Anatomy apps | `lead_group.m`, `lead_connectome.m`, `lead_anatomy.m` | **out of scope** for single-patient pipeline |

### 11.1 D3 — `ea_elvis` (Electrode-Scene)

**Figure name:** `Electrode-Scene` (or `resultfig` appdata) — `Name` set to patient/title when done building; during build `Name` may show `...building...`.

**Window decoration:** `Menubar` may be set **none** in subfigures; main scene uses default or custom menus in branches.

**Renderer:** OpenGL, `axis off`, `axis equal`, `Clipping` off on figure (legacy MATLAB).

**Keyboard:** `KeyPressFcn` / `KeyReleaseFcn` = `@ea_keypress` / `@ea_keyrelease` (camera / slice — implementation-specific).

#### 11.1.1 Toolbar (`uitoolbar` on `resultfig`)

| Control | Type | Action (reference) |
|---------|------|--------------------|
| Slice control | `uipushtool` | Tooltip **“Slice Control Figure”**; `ClickedCallback` → `opensliceviewer(resultfig, options)` (opens `ea_anatomycontrol`-class companion) |
| Contact labels | `uitoggletool` (conditional) | Toggle visibility of `eltext` (contact name labels) **On/Off** when single-patient and plans present |

(Additional **rotate** / **pan** tools are commented or toggled in different branches — verify `.m` for current set.)

#### 11.1.2 Menu: **Add Objects** (`uimenu` top-level on `resultfig`)

| Item | `ea_addobj` kind |
|------|------------------|
| Tract | `tract` |
| ROI | `roi` |
| Tract weighted by ROI | `tractmap` |
| Fiber activation result from OSS-DBS | `fiberactivation` |
| Electrode Localization | `reco` |
| VTA (MAT file) | `vat` |

#### 11.1.3 Patient content (when not “No Patient Selected”)

- Load recon → `ea_renderelstruct` → `ea_trajectory` / planning leads.
- Optional **LeGUI** paths add multiple trajectories.
- If `d3.elrendering==1 && d3.exportBB`, build `vizstruct` for BrainBrowser JSON export.
- Isosurfaces, VAT, cortex (`ea_showcortex`), atlas (`ea_showatlas`), etc., pulled in via prefs and stage flags (see the Phase A archive (removed 2026-10-23, in git history) section `specs/phase_a_visualization_report.md`).

#### 11.1.4 Group mode (`nargin>1` or `elstruct` from Lead Group)

- `multiplemode=1`, subset patients, optional mirror sides.

**Wyss DBS Localizer:** implement **single-patient** D3 first; **group** is separate milestone.

#### 11.1.5 Scene navigation (mouse) — behaviour contract (2026-10-14)

Lead-DBS installs its own figure callbacks for the scene camera (a figure-level button-down / motion / scroll-wheel handler, `helpers/gui/ea_mouse_camera.m`); it does **not** use MATLAB's `rotate3d` or `cameratoolbar` modes, has **no keyboard camera control** and **no preference** for the bindings. The scene camera is orthographic in the stored default view; the orbit centre is the camera target, which only pan and the default-view reset move.

| Input | Lead-DBS effect | Gain |
|-------|-----------------|------|
| Left drag | Orbit about the camera target; vertical axis fixed to world **z** (superior) so the head never rolls; horizontal drag = azimuth, vertical drag = elevation | **1° per 7 px** of pointer travel, each axis |
| Right drag, or Ctrl + left drag | Pan: camera and target move together in the view plane, the scene follows the pointer | **half** the pointer distance |
| Middle drag, or Shift + left drag | Zoom (view angle); drag **right or up** = zoom in | factor `1 + clamp((dx + dy_up) / 70, −0.9, 0.9)` per motion event |
| Mouse wheel | Zoom toward the view centre (not toward the pointer); wheel **toward the user** (scroll down) = zoom **in** | factor `1 + n / 10` per notch in, `1 / (1 + n / 10)` out |
| Double click (any button) | Reset to the saved default view (camera, slices) | animated in Lead-DBS |
| Keyboard | none for the camera | — |

**Wyss DBS Localizer (`dbsloc_viz::gesture_zoom::NavigationScheme`):** two schemes, selectable in the 3D Viewer's *Mouse & touchpad* section and persisted by the GUI (`DbslocAppState.navigation`).

- **`LeadDbs` (default):** the table above. Zoom acts on the orbit distance (the viewer is perspective): a Lead-DBS zoom factor `q` divides the distance. Wheel: one notch toward the user divides the distance by 1.1; one notch away multiplies it by 1.1. Drag zoom: `q` from the table with egui's screen delta (`dy_up = −delta.y`). Pan: half the pointer-follow gain of the `Classic` scheme. Orbit: 1° per 7 points regardless of viewport size. Double click (any button) = *Reset view* (same as key `R`, not animated). Touchpad pinch / Ctrl + wheel still zoom (`zoom_delta`) because touch devices have no middle button; the existing keyboard shortcuts (`R`, `1`–`3`, Shift + arrows, …) stay.
- **`Classic`:** the Wyss bindings in force before 2026-10-14: left drag orbit (a full viewport height sweeps 180°), right / middle / Shift + left / Alt + left drag pan (scene follows the pointer), Ctrl + right drag zoom, wheel zoom with wheel **away** from the user = in, touchpad two-finger scroll pan, pinch / Ctrl + wheel zoom.

The help window (F1), the *Mouse & touchpad* panel section and the scene hover text describe the **active** scheme only. The `dbs-localizer scene` mode defaults to `LeadDbs`.

**Camera model (2026-10-14).** The orbit camera is a true z-up spherical camera, as in Lead-DBS: `azimuth` (°) is the horizontal rotation about world **z** in MATLAB's `view(az, el)` convention (0° = camera on −y looking anterior, 90° = camera on +x, 180° = camera on +y looking posterior), `elevation` (°) is the angle above the xy-plane, clamped to ±89°, and the camera up vector is world z. Eye = target + distance · (sin az · cos el, −cos az · cos el, sin el). The *Rotation (°)* and *Elevation (°)* sliders show exactly these two angles. (Before this date the orbit tilted about world y, so the *Elevation* slider read 80° for a 10° view.)

**Projection.** Two projections, `Orthographic` (default, as in Lead-DBS) and `Perspective` (45° vertical field of view, the previous behaviour), selectable in the *View* section and persisted by the GUI. In both the `distance` value is the zoom: the visible half-height at the target plane is `distance · tan(22.5°)`, so a given distance frames the same height in either projection, and pan / dolly gains do not change. In the orthographic projection the eye is placed far from the target (the look-at direction only), so nothing between the real camera distance and the target is clipped.

**Default view (Lead-DBS `spacedef.view` of MNI152NLin2009bAsym; *Reset view* restores it):**

| Parameter | Lead-DBS | Wyss DBS Localizer |
|-----------|----------|--------------------|
| Azimuth | 180° | 180° |
| Elevation | 21° | 21° |
| Projection | orthographic, view angle 9.26° at 490 mm = 79.3 mm visible height | orthographic, `distance` = 79.3 / (2 · tan 22.5°) ≈ **96 mm** |
| Target (orbit centre) | (−3.16, −25.27, −3.65) mm | the same point in world mm when it lies inside the template volume, else the template centre |
| Up | world z | world z |

Slice defaults: §11.1.7. Persisted `SceneViewState` snapshots carry `camera_convention = 1`; a snapshot without it (written before this change) restores atlas and contrast but not the camera, which falls back to the default view.

#### 11.1.6 Refine localization view — behaviour contract (2026-10-14)

The expert's correction tool of `05` §9a. Reference behaviour (Phase A archive §5.2, removed 2026-10-23, in git history): slices of the post-op image with the lead drawn on them, the user picks contact 0 (head) or contact 3 (tail), nudges it with the arrow keys, rotates the lead, finishes a side with Space. The Wyss view keeps that model.

**Component.** `dbsloc_viz::refine_app::RefinePanel` (feature `refine-embed`, library only; the GUI embeds it in the *Refine localization* tab, pop-out with F11 like the other viewers). Input `RefineRequest` (serde): `subj_id`, `space` (`native` | `mni`), `backdrop_nii` (the image of `05` §9a *Edit space*), `sides: [{key: "1" | "2", label: "Right lead" | "Left lead", elmodel, head, tail, x, y, roll_deg, candidates_deg, orientation_status, manually_corrected, review}]` (`review` = `"corrected"`, `"accepted"` or `null`, `05` §9a *Per-side review*) — coordinates in the edit space, millimetres. The pipeline builds it (`manual_correction::prepare_manual_correction(sub, opts) -> ManualCorrectionContext`, same field names); the panel never reads a subject folder. Output `RefineOutput { save: Option<Vec<SideEdit {key, head, tail, roll_deg: Option<f64>, accept: bool}>>, cancelled, errors }`: the host applies the edits with `apply_manual_correction` off the paint thread; the panel never writes files. `roll_deg` is `Some` only when the user changed the roll. `accept: true` marks a side accepted without an edit (`05` §9a); its `head` / `tail` are the file's markers and are not read by the pipeline.

**Layout.** Three slice panes of the backdrop (axial on the left half, coronal and sagittal stacked on the right; voxel-aligned planes of the backdrop grid, physical aspect, same pane mapping as the registration review) plus a controls column; `1` / `2` / `3` show a single pane, `0` restores the three-pane layout. Window / level, zoom and pan behave as in the registration review (Shift + left drag = window / level, right drag / pinch / Ctrl + wheel = zoom, middle or Alt + left drag = pan, `R` = reset zoom / pan). Default window for a CT backdrop: level 300 HU, width 1500 HU (bone / metal); for an MR backdrop the 0.5–99.5 percentile range.

**Overlay (egui paint over each pane, from the live geometry of `05` §5 `resolve_fiducials` of the current markers):** the lead axis (the 50-point trajectory projected into the pane), every contact centre as a circle of the contact radius at the pane scale, the **head** and **tail** handles as larger rings, the selected one filled with the accent colour; the segment-A direction (`y − head`) as a 5 mm arrow from the head in the pane; the other side drawn dimmed, not selectable. Points farther than 2 mm from the pane's slice are drawn at half opacity. A status line under the panes: `head shift`, `tail shift` (mm from the file's markers), `roll` (°, and `Δ` from the file), `tilt from vertical` (°), electrode model.

**Crosshair / slices.** The three panes show the slices through a crosshair. Selecting a marker, nudging it or dragging it moves the crosshair onto that marker; the wheel steps the hovered pane's slice away from it to inspect; `F` recentres on the selected marker. On open, after a side switch and on every recentre the panes zoom so that about 50 mm of in-plane extent fills the shorter pane side around the crosshair (the lead, not the whole head); the user's zoom is kept until the next recentre, and *Reset zoom and pan* (`R`) returns to zoom 1.

**Editing (per side, the current side chosen with `<` / `>` or the side tabs):**

| Action | Input |
|--------|-------|
| Select head (contact 0) / tail (the proximal fiducial) | `H` / `T`, or click within 12 pt of the handle in any pane (`0` and `1`–`3` stay the layout keys, as in the registration review) |
| Nudge the selected marker in the hovered pane's plane | `←` `→` `↑` `↓`: 0.25 mm; Shift = 1 mm; Ctrl = 0.05 mm (pane axes, so `↑` on the axial pane is anterior) |
| Move the selected marker through the pane plane | `PageUp` / `PageDown`, same steps |
| Drag the selected marker | left drag on its handle; through-plane coordinate unchanged |
| Move the whole lead (head and tail together) | the same keys with Alt, or dragging the lead axis between the handles |
| Roll | `[` / `]` = −1° / +1°, Shift = 10°; slider and numeric field in the controls; *Flip 180°*; *Other candidate* (enabled when `candidates_deg` has two values); *Automatic* (back to the file's `roll_deg`) |
| Numeric entry | head and tail `x`, `y`, `z` and roll fields in the controls (mm, 0.01 steps; replaces the `m` dialog) |
| Undo / redo | `Ctrl+Z` / `Ctrl+Y` (per side) |
| Reset side | *Reset side* button (markers and roll of the file) |
| Accept side and go to the next | `Space` (*Accept side*): marks the side as checked; an edit of the side afterwards clears the mark |
| Save | *Save corrections* button (`Ctrl+Enter`): emits `save` for every side whose markers or roll changed, with `roll_deg` only when the roll changed, and an `accept: true` entry for every side accepted without an edit (2026-10-16); a side that was edited and accepted is saved as an edit |
| Discard | *Discard* button (`Esc` when nothing has focus): `cancelled` |
| Centre on the selected marker | `F` |

Plain keys are ignored while a text field has focus (`ctx.text_edit_focused()`, so a focused button does not swallow them), as in the 3D scene; the table is exported by `refine_app::shortcut_rows()` for the F1 help and listed in `docs/agent-rules/gui-ux.md`.

**Rules.** A side whose electrode model has ring contacts only (`is_directed == false`) shows no roll controls (one muted line *Ring contacts: roll not applicable*), never emits `roll_deg`, and its status line omits the roll. A side already corrected in the file is labelled *· corrected* on its tab, one accepted in the file *· accepted*, an accepted side not yet saved *· accepted, not saved*; an unsaved accept counts as a change for *Save corrections* and the discard question. The panel draws no heading of its own (the host shows the subject). The roll is kept when the head or tail moves (the panel re-derives `y` by projecting the previous `y − head` ⟂ the new axis, exactly as `05` §9a step 2, so what the panel draws is what the pipeline will write). A move that would make `head == tail` is refused (no change). Marker coordinates are never rounded by the panel. Both sides may be edited before one save. The host shows a *Manually corrected* chip on the Reconstruct card afterwards, one line per lead with its review state (*corrected*, *accepted*, *unchecked*, or *not found* for a side the reconstruction left out, `05` §4.4) and *vertical segment marker* when `meta.reconstruct.flags` carries `vertical_marker` (`06` §10.6), reloads the 3D scene, and warns on that card that *Rerun all selected steps* traces every lead again and drops the corrections and acceptances.

**Acceptance.** (a) Pane mapping round trip: a world point → pane uv (`plane_uv_for_world(plane, world, slice_index)`, with the signed through-plane distance in mm) → world point (`world_for_plane_uv(plane, u, v, slice)`) within 1e-4 mm on an oblique backdrop affine. (b) `→` on the axial pane moves the selected marker by (+0.25, 0, 0) mm in the backdrop's voxel `i` direction mapped to world (for a RAS-aligned affine: +x); Shift / Ctrl scale it. (c) After moving the head 3 mm along the lead axis, or with a roll set, the roll reported by the panel equals the previous roll within 1e-6° (a lateral move changes the axis, so the projected roll changes geometrically, as in `05` §9a step 2). (d) `]` ten times = +10° roll; *Flip 180°* twice = identity. (e) `save` lists only the edited sides, with `roll_deg: None` when only markers moved; a side accepted without an edit is listed with `accept: true`, an edited and accepted side with `accept: false`. (f) `Ctrl+Z` after one nudge restores the previous markers exactly. (g) `DBSLOC_APP_SCREENSHOT_ACTION=refine` with `DBSLOC_APP_REFINE_REQUEST=<RefineRequest json>` over synthetic data renders the tab (never a real subject).

**Lead-DBS parity amendments (normative, 2026-10-22; reference behaviour `specs/12` Part 2, gaps of its §12 closed: 3, 4, 5, 8, 9, 10, 13, 14, 17, 19, 21).** The *Lead planes* view is the default (`L`): two panes that contain the lead axis, **Lead from the front** (axis + world x) and **Lead from the left** (axis + world y), resampled trilinearly from the backdrop along the current trajectory (head − 5 mm to tail + k·catalogue span, k = 2, or 1.5 for more than 8 contacts; ±10 mm across; 0.2 mm steps; resampled only when the markers change, synchronously — 512³ CT: ≤ 5 ms, ≤ 11 ms with the slab), the lead axis pointing up, shown side by side or stacked, whichever gives more pixels per millimetre; beside them **Head, axial** and **Tail, axial** close-ups (±15 mm in the backdrop's own i/j plane through each marker; each close-up offers only its own marker for selection); the three voxel-aligned panes of the layout above become the *Three planes* view (`0`) and the single-pane views (`1`–`3`). Every pane zooms and pans; R/L, A/P (and S/I on the orthogonal panes) captions sit at the image edges. The lead panes and the close-ups draw only the current side. Dragging a handle in a lead pane freezes that pane's plane until release so the handle follows the cursor; the other panes follow live. Arrow keys in a lead pane move across the lead (`←` `→`) and along it (`↑` `↓`), `PageUp` / `PageDown` through the plane. **Keep lead length** (checkbox, `K`, default on): an in-plane nudge or drag of one marker recomputes its coordinate along the lead axis so that |tail − head| is unchanged (the moved marker pivots about the other and stays on the same side of it; a move longer than the lead stops at the lead length with a status note; a move purely along the axis is refused with a note pointing to the along-axis move; typed coordinates ignore the option). **Move the whole lead along its axis:** `Alt+PageUp` (proximal) / `Alt+PageDown` (distal), 0.25 mm, Shift 1 mm, Ctrl 0.05 mm, roll unchanged (replaces the earlier Alt through-plane move; `Alt` + arrows still move the whole lead in the hovered pane). **Move whole lead** checkbox beside the numeric head / tail fields: editing one marker translates the other by the same vector. **X-ray (slab)** (`X`): the lead panes show the mean of 9 parallel planes at −4 … +4 mm along the pane normal; display only, saved markers unchanged. **Hide markers** (`V`): hides handles, contacts, axis and the segment-A arrow in every pane; keys still act. Status line adds `Spacing: s mm (catalogue c mm, ratio r)` = the mean adjacent level distance of the marker-implied contacts (`05` §6 stretched construction, so it tracks the head–tail distance); an edit never touches the other side. A static model diagram (catalogue levels, *Head (contact 0)* and *Tail* labelled) is shown when the controls column has spare height. Window / level and zoom controls live in a collapsed section; *Save corrections* and *Discard* are pinned at the bottom; the view fits a 1280×720 pop-out without scrolling. **Roll convention:** the panel's roll is the horizontal azimuth of `06` §10.6 (`specs/12` §11.5): x̃ = (cos θ, sin θ, 0) projected ⟂ the lead, y_dir = t × x_dir, θ = 0 → y anterior; read-back = the unsigned angle between anterior and the horizontal projection of y − head, negative when y lies on the +x side of head; a vertical lead is unchanged, a 30° lateral tilt at θ = 45° differs by 4.1° from the former axis rotation. `SideEdit.roll_deg` and the request's `roll_deg` / `candidates_deg` are in this convention (the pipeline writes the same convention, `05` §9a step 2). Acceptance (`specs/12` §13 items 1–7 as `refine_app::tests`): a 30°-tilted bright cylinder appears as a straight band along the lead panes; keep-lead-length holds within 1e-9 mm over 20 random nudges; the along-axis move shifts head and tail by the same vector; the spacing readout equals the catalogue spacing for a catalogue-length lead and grows 10 % with a 10 % stretch; the roll convention agrees with the azimuth formula within 1e-6° for a vertical lead; slab mode and the hide toggle leave the markers byte-identical.

#### 11.1.7 Anatomy slices and backdrop — behaviour contract (2026-10-22)

Reference behaviour (Phase B reading of the reference implementation's *Anatomy Slices* control and the `spacedef` of MNI152NLin2009bAsym, 2026-10-22): the three orthogonal slices show one **backdrop** image; the default slice position is a fixed point of the space, not derived from the leads; a popup lists the backdrops (the space's templates, the patient's images, downloaded high-resolution backdrops, *Choose…*).

**Default slices.**

| | Reference | Wyss DBS Localizer |
|-|-----------|--------------------|
| Position (world mm) | x = −30, y = −30, z = −30 | the same, in every template space |
| Shown | sagittal, coronal | sagittal, coronal (axial hidden) |
| Opacity | 100 % each | 1.0 each |

The slice position is a world-mm point; each plane's voxel index in the current backdrop is `round(world_to_voxel(p))` on its axis, clamped to the volume. When the point lies outside the backdrop's extent on an axis (a cropped backdrop), that plane uses the backdrop's centre index ⌊n / 2⌋ on that axis. The viewer keeps the exact world position of each plane (`slice_world`), not only the index: it changes only when the user moves that plane (slider, keyboard) and is then set to the plane's world coordinate in the current backdrop; switching backdrop, saving and restoring map this stored position, so a plane that falls back to the centre of a cropped backdrop returns to its old position when the user switches back, and repeated switches do not drift by grid rounding. *Reset view* restores these defaults. The previous lead-tip focus rule is removed. The side panel shows each plane's world coordinate next to its index (e.g. *Index 104 · y −30.0 mm*).

**Persistence.** `SceneViewState` stores the slice positions as world mm (`slice_mm: [x, y, z]`), the three visibility flags, the opacities and `backdrop_id`, with `slice_convention = 1`. A snapshot without `slice_convention` (written before this change) restores camera, atlas and contrast but not slice positions or visibility, which take the defaults above. Restored world positions map to the current backdrop's grid with the rule above, so they survive a backdrop change.

**Reference template vs backdrop.** The manifest's `template_nii` stays the scene's reference image: atlas meshing, scene bounds, default camera target and the mesh cache key use it, never the backdrop. The backdrop is only what the slices show. The slice quads span the backdrop's own grid (a cropped backdrop draws smaller planes).

**Backdrop list** (combo *Backdrop* at the top of the *Slices* section), in this order:

1. **Template images** of the scene's template: `<space> T1w (template)` = `template_nii`; `<space> T2w (template)` when `t2.nii.gz` or `t2.nii` exists beside it. Kind `template`.
2. **Patient images in template space**, each listed only when its file exists (`specs/01` §7a names, `derivatives.rs`): `Patient post-op CT (normalized)` (`norm_postop_image(sub, "CT", space)`), `Patient post-op MRI, axial / coronal / sagittal (normalized)` (keys `ax`, `cor`, `sag`), `Patient pre-op <modality> (normalized)` (`norm_anchor_image(sub, space)`). Kind `patient`.
3. **Installed downloadable backdrops** of the scene's space (catalogue below). Kind `download`. In the GUI, catalogue entries of that space that are not installed are listed too, labelled `<title> — download (NN MB)`.
4. **Choose file…** (GUI only): any NIfTI file, which must already be in the scene's template space; kept as kind `custom` with id `custom:<absolute path>`.

The list is built by one pipeline function `dbsloc_pipeline::backdrops::scene_backdrops(sub, template_t1, space) -> Vec<BackdropOption { id, label, path, kind }>` (ids: `template-t1`, `template-t2`, `patient-postop-ct`, `patient-postop-ax` / `-cor` / `-sag`, `patient-preop`, catalogue id for downloads). The Visualize stage writes it into the scene manifest as `backdrops: [{ "id", "label", "path", "kind" }]` (absolute paths; optional field, `#[serde(default)]`); the GUI calls the same function when it loads a scene, so manifests written before this change also get the full list. The standalone viewer uses the manifest list, or `template_nii` alone when the field is missing.

**Default backdrop:** `template-t1`. The persisted `backdrop_id` is used when that id is in the list; otherwise `template-t1`. **Atlas set (2026-10-22, `11`):** the persisted `atlas_id` is selected when the loaded scene lists it; otherwise the scene's `atlas_default_id` (DISTAL Minimal) is shown and the saved id is kept until the user picks another set, so a scene without the external atlas does not overwrite the choice.

**Switching.** Choosing an entry loads the image on a worker thread (*Loading backdrop…* with a spinner under the combo); camera, atlas, slice world positions, visibility and opacities are kept; window / level returns to its default (1.0 / 0.5) because the new image has its own intensity range. A failure (unreadable file, loader dimension guard, wrong file) shows one error line under the combo and keeps the previous backdrop. A backdrop whose world extent does not overlap the template's shows the warning *This image does not overlap the template; is it in <space>?* but is drawn. A backdrop whose voxel axes are not in x / y / z order (for each voxel axis i, the largest absolute sform / qform component of its column must lie on world axis i; flips allowed) is refused with *This image's axes are not in x / y / z order; resample it to the template first.* and the previous backdrop is kept. At most one backdrop load runs at a time: a choice made while a load runs is queued (a later choice replaces the queued one) and starts when the running load ends; its result is dropped when another choice was made meanwhile. Choosing the backdrop already shown while a load runs cancels the queued choice. A restored `backdrop_id` that is not yet in the list (or still loading) is what the next save writes, so a pending choice is never overwritten by `template-t1`. At most one backdrop volume besides the reference template is kept after a load completes (the template T1 backdrop shares the reference volume). Intensity normalisation is the existing one (0 → the 99.5th percentile of the non-zero voxels).

**Backdrop catalogue** (`crates/dbsloc-pipeline/src/backdrop_catalog.rs`, the only place naming backdrop sources; fetch `backdrop_fetch.rs`). Entries for MNI152NLin2009bAsym:

| id | Title | Source file (OpenNeuro ds002179, `https://s3.amazonaws.com/openneuro.org/ds002179/derivatives/sub-EXC004/processed_data/MNI/…`) | Bytes | SHA-256 | Install |
|----|-------|------|------:|---------|---------|
| `edlow2019-7t-exvivo_res-0.5mm` | 7T ex vivo brain (Edlow 2019), whole brain, 0.5 mm | `Synthesized_FLASH25_in_MNI_v2_500um.nii.gz` | 74 205 499 | `a47e6fbf50c0043272ca674d461d90a6346083ac7a66e8eb2277d4fff279691b` | copied verbatim |
| `edlow2019-7t-exvivo_res-0.2mm_desc-deepbrain` | 7T ex vivo brain (Edlow 2019), deep brain region, 0.2 mm | `Synthesized_FLASH25_in_MNI_v2_200um.nii.gz` | 1 154 496 050 | `acdbb81d5d3b6c793cb05ed520baad61db4eac6305e6e024d45ddb9d920e76c1` | cropped to x ∈ [−50, 50], y ∈ [−60, 35], z ∈ [−40, 35] mm |

Source facts (headers read 2026-10-22): float32, sform = qform code 1, axes aligned with RAS; 0.5 mm: 395 × 467 × 379, origin (−98.2, −134.2, −72.2); 0.2 mm: 986 × 1166 × 946, origin (−98.05, −134.05, −72.05). The 0.1 mm file (7.3 GB, 1971 × 2331 × 1891) is not offered: it exceeds the loader guard and typical free disk. The reference implementation's "100 µm" backdrop is the 0.2 mm whole volume read memory-mapped; Wyss offers the whole brain at 0.5 mm and the DBS region at 0.2 mm instead, because the viewer holds its volume in memory. Data licence: CC0 (Dryad and OpenNeuro records). Citation, written to the installed `LICENSE` and shown in the GUI download row: *Edlow BL, Mareyam A, Horn A, et al. 7 Tesla MRI of the ex vivo human brain at 100 micron resolution. Sci Data 6, 244 (2019), doi:10.1038/s41597-019-0254-8; data: OpenNeuro ds002179, doi:10.18112/openneuro.ds002179.v1.1.0.* The OpenNeuro mirror is used because the Dryad API refuses unauthenticated downloads.

**Fetch** follows `specs/04` §3.2 (HTTPS GET streamed to a staging folder with incremental SHA-256, progress at most every 1 MB, cancellation between chunks, body limited to the pinned size + 1 MB, size and SHA-256 must equal the pins, atomic rename onto the destination, old copy kept aside until the new one is in place, staging removed on failure or `cancelled`), with these differences:

- Staging `<data>/backdrops/space/<space>/.partial-<id>-<pid>-<n>/download.nii.gz` (`n` a per-process counter, so two fetches in one process never share a folder); a fetch first removes `.partial-<id>-*` folders of that space older than 24 hours (left by a killed process); destination `<data>/backdrops/space/<space>/<id>/` with `backdrop.nii.gz`, `LICENSE` (licence and citation text from the catalogue) and `backdrop.json` (`id`, `space`, `title`, `res_mm`, `source_url`, `sha256`, `crop_mm` or `null`, `fetched_by`). Installed = `backdrop.nii.gz` exists.
- Before the download, free space on the data folder's volume must be at least pinned size + expected output size + 512 MB; otherwise fail with `not enough free disk space for <id>: need <N> MB, <M> MB free` before any byte is fetched.
- **Crop** (entries with a crop box): the downloaded file is stream-decompressed once; the header must have the pinned dimensions and datatype float32 and an axis-aligned positive-diagonal sform (else fail, nothing installed). The voxel range on each axis is `ceil((lo − origin) / pixdim)` … `floor((hi − origin) / pixdim)`; only the z slabs in range are kept, each cropped in x / y; the output is float32 with the same voxel size, sform and qform translated to the first kept voxel (codes kept), `scl_slope` / `scl_inter` copied, written **uncompressed** as `.nii` (amended 2026-10-25: the float crop compresses only about 1.17×, and the uncompressed file switches about 0.8–1 s faster; an earlier `.nii.gz` install is still read). The download is deleted after the crop. For the 0.2 mm entry the voxel ranges are i 241…740, j 371…845, k 161…535, so the output is 500 × 475 × 375 voxels (origin (−49.85, −59.85, −39.85) mm).
- CLI: `dbsloc backdrop list` (id, space, title, size, state) and `dbsloc backdrop fetch <id>` (progress lines, Ctrl-C cancels, exit 0 / 1 / 130).

**GUI.** The 3D viewer's combo lists the entries above. Choosing an entry that is not installed shows, in the scene panel, the title, size, citation and *Download (NN MB)*; then a progress bar with *Cancel*; on success the backdrop is selected. The download runs on a worker thread; closing the application cancels it. The chosen `backdrop_id` is saved with the view state.

**Acceptance.** (a) Fresh view on the shipped 1 mm template: sagittal index 68, coronal 104, axial 42 (x = y = z = −30 mm), sagittal + coronal shown, axial hidden. (b) The same position on the 0.5 mm Edlow grid: indices 136, 208, 84. (c) Switching backdrop keeps the camera and the slice world positions; a snapshot without `slice_convention` gets the defaults. (d) Crop of a synthetic float32 volume (known values, known origin) yields the expected dims, sform translation and voxel values; a header with other dims is refused. (e) Catalogue pins non-empty; fetch refuses a size / SHA-256 mismatch and cleans the staging folder (local test server or file-backed test hook, no network in tests). (f) `scene_backdrops` lists exactly the existing files in the order above.

#### 11.1.8 Fibre and map layers — behaviour contract (2026-10-25)

Owner decisions of 2026-10-25. Both layer kinds are pushed by the application (never read from the scene manifest in v1) and are not persisted.

- **Fibre layer** = id, label, the fibres (world-mm points in template space, per-fibre offsets), one value per fibre, a colouring and the total fibre count N of the source. Sources: the connected fibres of a stimulation (`15` §7.4 / §9.2 `_fibers.mat`, colouring *strength*: white → `[0.9176 0.2 0.1373]` over 0 … max |value|), a fibre-filtering result (`14` §8.6 `fibres_R/L.mat`, colouring *signed*: the §8.3 positive / negative gradients, white at the smallest |value| of the shown fibres of that sign and the peak colour at the largest, so a result that keeps only the top fibres still spans the full gradient; the side panel shows the range), and any fibre file the user adds (*Add fibre file…*, `.mat` §3.2 or `.trk`, uniform colour, chosen per layer).
- **Drawing.** Fibres are screen-facing ribbons of constant pixel width (setting 1–6 px, default 2 px, scaled by the display's pixels per point), one colour per fibre, opaque, depth-tested and depth-writing, drawn in the opaque pass after the electrodes and before the slices; translucent VTA, atlas and map surfaces (OIT pass) therefore tint the fibres behind them. Fibre geometry has its own vertex buffer and generation: adding, removing or re-colouring a fibre layer never re-uploads atlas or electrode meshes.
- **Decimation.** Per layer at most *Maximum fibres shown* (default 200, slider up to 100 000), chosen by descending |value|, ties by ascending fibre index (`15` §7.4); the panel shows *n of N fibres shown* per layer.
- **Side panel.** A *Fibres* section lists the layers (visibility, colour swatch for uniform layers, *n of N*, remove) with the shared width and maximum controls and *Add fibre file…* (only when the hosting application supplies a reader; hidden in the standalone `dbs-localizer scene` mode).
- **Map layer** = id, label, a template-space scalar volume (dims, affine, values) and a display mode. Default mode *Isosurface*: positive and negative parts meshed at ± threshold after light smoothing, coloured with the `14` §7.7 peak colours, translucent through the OIT pass (α 0.5), meshed on a worker when the threshold changes. Alternative mode *Slices*: the values painted on the visible anatomy slices with the `14` §7.7 two-sided gradient, voxels below |threshold| transparent. The threshold slider spans 0 … max |value| (default 25 % of max |value|). A *Maps* section lists the map layers (visibility, mode, threshold, remove).

#### 11.1.9 GUI locks — runs, image import and the stimulation editor (2026-11-12)

Normative for `dbsloc-app`. Every refusal named below shows its reason in an error window (`docs/agent-rules/gui-ux.md` *Error display*), not only in the log, unless stated otherwise; where a control is said to be disabled, the reason is also its disabled-hover text.

- **Run → subject and output directory.** While a run is active the dataset and the selected subject cannot change (`Ctrl+O` and `Alt+↑` / `Alt+↓` during a run only write the reason to the log); while a run, the registration review, *Refine localization* or the stimulation editor holds results of the output directory, the output directory cannot change.
- **Starting a run.** A run is refused, with the reason in a *Cannot start the run* window, while a run or a run of several subjects is active, while a wait reason stands (review images being prepared, the clean-up after a registration review still running, a lead correction being opened with the *Refine localization* button (not one the tab opens while following the selection), unsaved or being saved, an image import writing), or when no subject is selected. This holds for every way to start one: *Run* (one or several subjects), `Ctrl+R`, the overwrite confirmation, the guided *Run this step*, *Rerun this step*, *Open 3D viewer* and *Compute VTA* (the latter in a *Cannot compute the VTA* window); a run of several subjects refused for an unmet setting or an active batch reports it in a *Cannot start this run yet* window. `Ctrl+R` does nothing on the start screen and while the import window is open. No refused run moves the focus to the *Output* tab. Preparing a registration review while a run is active, or while review images are still being prepared, is refused in a *Registration review* window.
- **Image import ↔ jobs that write into a subject (`01` §8.4).** The *Import images* wizard does not open while any of these is active, each with its own reason: a run; a run of several subjects (including its pending reviews); a registration review being prepared or open; the clean-up after a registration review still running in the background (the decision itself is written when *Approve* / *Reject* is pressed); *Refine localization* being opened with its button, or open; a lead correction being saved; the group analysis *Compute VTAs* job, counted until its worker thread has really ended (also after *Close* in the group panel, and also when it panicked). The GUI jobs left out of this lock, and why, are listed in `01` §8.4: stimulation *Save* and *Compute connectivity*, the group *Analysis* jobs and the session-report import write neither raw images, `rawimages.json` nor stage outputs, approvals or locks. The header item *Import… → Images into a subject…*, the start-screen button and the guided Subject-step button are disabled; `Ctrl+I` shows the reason in an error window. While the wizard is open the same check runs every frame: *Import* and *Import anyway* (in the no-post-op question) are disabled, and if an import is still requested (a state that changed in the same frame) it is refused with the reason in the wizard's error window, before the no-post-op question opens; nothing is written and the wizard keeps every choice. Conversely, while the import job writes into the dataset: *Run* is disabled and a run started another way is refused; a batch does not start and its next subject waits until the import has finished; preparing a registration review, opening *Refine localization*, saving a lead correction and the group *Compute VTAs* are refused with "Wait until the image import has finished writing into the dataset."; the stimulation editor's *Compute VTA* is disabled with that reason; closing the window asks first, because quitting can leave the subject's raw data half-written: *The image import is still writing* with *Keep importing* (forget the close request), *Cancel the import, then quit* (signals the wizard's cancel, whose rollback restores the subject being written; the window then says so and only *Quit now* stays enabled) and *Quit now*. If the import ends cleanly while the question is open (every subject imported, or cancelled with a complete rollback), the application honours the close request and closes, unless unsaved lead corrections exist, which then get their own question. If it ends otherwise (a subject failed, the rollback after a cancel was incomplete, or the import stopped unexpectedly), the close request is set aside: the question closes, the application stays open with the wizard's error or result list visible, and the problem is written to the application log. A single-subject cancel whose rollback was incomplete is shown in the wizard's error window, not as the *nothing was written* notice; in a run of several subjects it is shown in the error window above the result list. DICOM conversion into the temporary folder holds back nothing: it writes nothing into the dataset.
- **Stimulation editor bound to its subject (`07` §7.1).** The editor belongs to the dataset directory and subject it was opened for; its title shows `Stimulation · sub-<id>`. The subject list stays usable while it is open. *Save* always writes the editor's subject. The editor follows the selected subject by the next rule; while it still shows another subject than the selected one (no subject selected, or the selected subject (`sub-` prefix ignored) or the dataset directory differs from the editor's), it shows the note of that rule, and for an editable label *Compute VTA* is disabled with "This editor belongs to sub-<id>, but another subject or dataset is selected" (or "…but no subject is selected") as the reason. *Compute VTA* is also disabled while a run or a run of several subjects is active (each with its own hover text) and while a wait reason stands (that reason as hover text). A compute request that still arrives is refused in a *Cannot compute the VTA* window (for another subject: naming the editor's subject and saying to select it again or to close the editor and open it for the subject wanted; otherwise: the run's refusal reason); nothing runs and the editor's waiting state and its *Computing…* status are cleared. A finished `vat` step loads VTA meshes into the editor only when the run's subject and dataset directory are the editor's; the end of a run, including one that stopped without reporting its end, clears a waiting state that was not answered.
- **Stimulation and Refine localization follow the selected subject (2026-11-12).** Once opened, each tab is for one dataset directory and subject (title `Stimulation · sub-<id>`, `Refine localization · sub-<id>`) and checks every frame whether that is still the subject highlighted in the subject list (`follow::decide`); the 3D viewer already reloads on the same change. Only the highlighted subject counts: ticking subjects for a run of several subjects moves neither tab. The rules, in this order:
  1. **Same subject**, or the tab holds nothing (never opened, closed with *Close* / *Discard*, or closed with the tab's ✖ while it showed only an empty state or was still opening a lead): nothing changes. Exception: an empty state of the selected subject is tried again once the subject's step status has been read again (a step or run finished, a review decision, *F5*, the window regaining focus), when no run, run of several subjects or job of rule 2 holds it.
  2. **A job of the tab's subject, or any run.** While a run or a run of several subjects is active (the batch moves the selection itself), or a job of the tab's own subject is running (stimulation: a *Compute VTA* waiting for its run, the VTA meshes loading, a connectivity job; Refine localization: a lead being opened or saved, review images being prepared or cleaned up, an image import writing), the tab keeps its subject and shows a warning note "<Tab> still shows sub-<a>, but sub-<b> is selected. It follows the selection when the job in progress has finished."; no buttons (a tab showing an empty state says "sub-<b> is selected. <Tab> follows the selection when the job in progress has finished." under it). The job is never moved to the other subject; the tab applies the rules again when it has ended.
  3. **Unsaved changes** (stimulation: settings that differ from the saved file, or a label created by *New…* / *Duplicate* not saved yet; a label the editor created by itself because the subject had no saved settings counts as unsaved only once it is edited, and following never writes it; Refine localization: a moved lead or an accepted side not saved): the tab keeps its subject and shows a warning note naming both subjects with three buttons: *Save and switch to sub-<b>* (writes the tab's own subject: the stimulation file at once, disabled with its reason while the settings have errors; the lead correction as *Save corrections* does, the tab switching once the save has succeeded and asking again when it failed), *Discard and switch to sub-<b>* (nothing is written) and *Stay on sub-<a>*. With no subject selected the first two read *Save and close* / *Discard and close*. *Stay* keeps the note with the two other buttons and is not asked again for that selection; after a save the note offers *Switch to sub-<b>* instead of switching by itself. After *Discard*, until the tab has switched (rule 2 or the step status still being read can delay it), the discarded settings are shown read-only with "The unsaved settings were discarded and stay read-only until the tab switches; select sub-<a> again to get them back.", and the discarded lead is not shown, with "The unsaved corrections of sub-<a> were discarded; select sub-<a> again to get them back."; selecting the tab's own subject again removes the note and undoes the unfinished *Discard* (the edits are still there). Nothing is discarded or applied to the other subject without one of these buttons; the 3D viewer does not draw the stimulation editor's VTA while it shows another subject.
  4. **No subject selected**, nothing unsaved: the tab closes its subject and says "No subject is selected." with what to do.
  5. **Otherwise** the tab opens the selected subject once the subject's step status has been read after its last change (meanwhile "<Tab> shows sub-<a>; switching to sub-<b>…", or "Switching to sub-<b>…" under an empty state; this also keeps typing in the dataset directory field from opening a subject per keystroke), without taking the focus, leaving the guided run or the start screen: the stimulation editor as *Edit stimulation…* would (its first saved label, or a new one), Refine localization as *Refine localization* would. A lead being opened this way does not hold back a run or an image import (no wait reason, no import lock); if one starts before the lead is open, the lead read before it is not shown, even when the run or import has already ended by then: while it still runs the tab says "A run or an image import started while the lead was opening. The lead opens when it has finished." and tries again by rule 1; when it has ended the lead is read again as soon as the step status has been read. A lead that arrives after the selection moved on, or after the tab was closed, is dropped and never opened in a closed tab. A selected subject that cannot be shown (no reconstruction, settings or lead that cannot be read, the step's settings unreadable) gets an empty state naming it with the reason ("Stimulation · sub-<b>: the stimulation editor cannot be shown for this subject." / "Refine localization · sub-<b>: the leads cannot be shown for this subject."), never the previous subject; the error window of the explicit buttons is not used for this.

### 11.2 D2 — `ea_writeplanes`

**Figure name:** `erase(patientname,'sub-') + ': 2D cut views...'`, `NumberTitle=off`, **no** menubar/toolbar, white background, large fixed pixel position (legacy layout).

**Parameters (partial):** `options.d2.backdrop` + `ea_assignbackdrop` (patient vs MNI), `figvisible` (`on`/`off`), `svfig` (save PNGs or only build figure), `manualtracor` to restrict to one orientation.

**Output:** For each side / contact, PNG export paths under BIDS-structured dirs; optional atlas overlay when `d2.writeatlases` and atlas index present.

**Wyss DBS Localizer:** map to [§3 `write_planes`](#3-2d--write_planes) — this addendum only fixes **figure title and visibility** behavior for parity when `fig_visible=on`.

### 11.3 `ea_imshowpair` (shared)

**Figure:** black background, **maximized**, `MenuBar`/`ToolBar` **none**, drag–drop for extra NIfTIs, scroll wheel for slice, window/level (Lev/Win), mouse zoom region (`magnify` pattern).

**Channels:** 4D `Img`: for **coregistration**, `callingfunction` selects **wires** blend indices `3:5` (RGB joint channels); for **normalization dbs** / connectome, index `3` only.

**Wyss DBS Localizer:** reimplement or embed — spec detail in `specs/03_coregistration.md` §12.4.

### 11.4 Standalone product GUIs (not in single-patient Wyss DBS Localizer v1)

- **`lead_group` / `lead_group_connectome`:** group stats, 2D/3D batch — cross-ref when/if `dbsloc group` exists.
- **`lead_connectome` / `lead_mapper`:** connectomics + VAT fiber mapping.
- **`lead_anatomy`:** atlas / backdrop selection shell.
- **`ea_mercontrol` + NEX child GUIs** — MER; optional modularity.

**Spec rule:** do **not** require parity with these in Phase C **core** CLI; list as **deferred** in `AGENTS.md` Phase C module checklist when row added.

### 11.5 Revision

When Lead-DBS updates GUIDE/figure tags, re-diff `ea_checkreg.m`, `ea_elvis.m`, `ea_writeplanes.m` and adjust `specs/03_coregistration.md` §12 and this §11; bump “Last updated” below and the doc map timestamp in `AGENTS.md` if needed.

**Last updated:** 2026-11-12 (§11.1.9 GUI locks: runs, image import and the stimulation editor; Stimulation and Refine localization follow the selected subject)

---

*End spec 08.*
