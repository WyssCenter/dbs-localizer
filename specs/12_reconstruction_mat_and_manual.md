# 12 — Reconstruction `.mat` interchange format and the reference manual correction view

> Phase B specification written 2026-10-22 from observation of the reference implementation (no reference code). Part 1 is normative for the `.mat` export / import of the reconstruction (`fastwarp/crates/fastwarp-matfile`, `crates/dbsloc-pipeline/src/reco_mat.rs`); Part 2 is the behaviour reference for the gaps closed in the *Refine localization* view (`specs/08` §11.1.6). Decisions of the orchestrator are recorded in `docs/PHASE_HISTORY.md` (2026-10-22). Referenced as `specs/12` §n.


**Status:** Phase B draft (2026-10-22), written from the reference implementation (Lead-DBS 3.2.1, `.version.txt`; git `5b1008d`, 2026-03-02) as behaviour and data-format prose. No reference code is reproduced. Phase C implementers read only this document plus `specs/05`, `specs/06`, `specs/08`, `specs/09`.

**Conventions.** "Side k" uses the reference implementation's index: **1 = right hemisphere, 2 = left** (same as Reco v2 keys `"1"` / `"2"`, `specs/05` §8). MATLAB classes are named as MATLAB names them (`double`, `char`, `logical`, `cell`, `struct`). Shapes are rows × columns. "Unit" = unit vector. `r = lead_diameter / 2` (`specs/09`).

---

# PART 1 — The reconstruction `.mat` file

## 1. Location and naming

| Era | Path | Notes |
|---|---|---|
| BIDS (current) | `<dataset>/derivatives/leaddbs/sub-<id>/reconstruction/sub-<id>_desc-reconstruction.mat` | Path is fixed by the subject resolver; there is no session or space entity. |
| Legacy (pre-BIDS) | `<patient dir>/ea_reconstruction.mat` | The legacy → BIDS migration renames it to the name above. |

Siblings in the same folder (CT only): `sub-<id>_space-rawCT_desc-brainmask.nii` and `sub-<id>_space-anchorNative_desc-brainmask.nii` (PaCER masks). There is **no** JSON sidecar for the reconstruction. (A separate, derived file `clinical/sub-<id>_desc-reconstruction.json` is written only by the "programmer" export tool from the `.mat`; it is not read back by the pipeline.)

When a consumer finds no subject resolver it builds the path as `<root>/<patientname>/reconstruction/<patientname>_desc-reconstruction.mat`, where `<patientname>` is the subject folder name (`sub-<id>`). Lead Group and the "process report" use this form, so the **file name must be exactly** `sub-<id>_desc-reconstruction.mat`.

## 2. MAT container version

- The file holds **one variable, `reco`**. The writer calls a plain save with no format flag. The format is whatever the user's MATLAB default is: **v7** (Level-5 MAT with zlib-compressed elements) by factory default, **v7.3** (HDF5) when the user changed the MATLAB preference. Files of both kinds exist in practice. For example, the Lead-Tutor `desc-uiprefs.mat` files are v7.3, written on `MACA64`.
- Lead-DBS reads with an ordinary load, which accepts v6, v7 and v7.3 alike.
- **Export rule (Wyss DBS Localizer):** write **v7** (Level-5, compression optional). MATLAB R2006b and later read it, and so do Octave and scipy, and it has no HDF5 struct-array complications. **Import rule:** accept v5/v6/v7 and v7.3.
- Legacy (pre-`reco`) files store top-level variables `coords_mm`, `trajectory`, `markers`, `elmodel`, `manually_corrected` instead of `reco`. Only the MNI→native converter still understands them. Exporters never write this form.

## 3. The `reco` struct

`reco` is a `1×1 struct`. Its top-level fields are listed below. Field order carries no meaning.

| Field | Written when | MATLAB class / shape | Content |
|---|---|---|---|
| `props` | always | `1×S struct` (S = highest side index written, 1 or 2) with fields `elmodel`, `manually_corrected` | per side |
| `native` | always for CT / native-first methods; derived from `mni` otherwise | `1×1 struct` (§3.2) | coordinates in **anchor-native** space (the pre-op anchor's world mm, after coregistration, **before** brain-shift correction) |
| `mni` | always after a "hybrid" save | `1×1 struct` (§3.2) plus `trajvector` | coordinates in the **template space** of the Lead-DBS installation (`MNI152NLin2009bAsym` by default) |
| `scrf` | only if the brain-shift transform file `brainshift/transformations/sub-<id>_from-<anchor>_to-<anchor>BSC_desc-scrf.mat` exists; removed otherwise | `1×1 struct` like `native` | `native` with the 4×4 brain-shift matrix applied to coords, trajectory and all four markers |
| `acpc` | only if user preference `reco.saveACPC` = 1 (default 0) | `1×1 struct`: `coords_mm` (`1×S cell` of `N×3 double`), `ac`, `pc`, `msp` (believed `1×3 double` mm each, **uncertain**) | contacts mapped to AC/PC (mid-commissural) coordinates |

Not stored in `reco`: electrode geometry (only the model name; geometry comes from the model catalogue, `specs/09`), stimulation settings, the reconstruction method, the software version, dates, the template space name, and the roll as a number. **The roll is encoded only through the `y` marker.**

### 3.1 `props(k)`

| Field | Class | Value |
|---|---|---|
| `elmodel` | `char` row, e.g. `'Boston Scientific Vercise Directed'` | Must be a model name that the Lead-DBS catalogue resolves (`specs/09` names). On load, the substring `St. Jude` is replaced with `Abbott`. |
| `manually_corrected` | `double` scalar 0 or 1. The writer passes numeric literals; `logical` is tolerated by every reader. | 1 = expert-approved (see §5.3). |

The writer fills `props(k)` only for the sides it handles. When only side 2 exists, MATLAB auto-creates `props(1)` with both fields as `0×0 double` (empty). The same model string is written for every handled side.

### 3.2 Space blocks `native`, `mni`, `scrf`

| Field | Class / shape | Content |
|---|---|---|
| `coords_mm` | `1×S cell`; element k = `N_k×3 double`; empty `[]` for an absent side | Contact centres, one row per contact in catalogue order (row 1 = most distal). PaCER-native output has one row per **level** (4 rows for an 8-contact directional lead) until something re-resolves it. |
| `trajectory` | `1×S cell`; element k = `M×3 double` | Straight polyline along the lead. **M = 50** when produced by the marker → contacts routine (§4) and always in `mni`. A raw PaCER native result has M = 20 (a different construction). Readers must not assume 50 in `native`. |
| `markers` | `1×S struct` with fields `head`, `tail`, `x`, `y`, each `1×3 double` (**row** vectors; readers concatenate them horizontally) | `head` = centre of the most distal model fiducial (contact 0 / level 1), `tail` = the model's proximal fiducial; `x`, `y` = points at distance `r` from `head`, perpendicular to the lead. Absent side: all four fields `[]`. |
| `trajvector` | `1×S cell` of `1×3 double` (unit, head → tail). **Only in `mni`.** | |

`x`/`y` normalisation: a native-first save rescales `x − head` and `y − head` to length exactly `r` (directions kept). A template-first save does not.

## 4. How the derived quantities are built (reference behaviour)

**Markers → contacts ("resolve").** The model file gives four model fiducials `head_position`, `tail_position`, `x_position`, `y_position` and the model contact centres. The **exact affine** (not rigid) mapping the four homogeneous model fiducials onto the four homogeneous markers is solved as a 4×4 linear system and applied to every model contact centre. This is the same map as `specs/05` §6 *Template-space contacts from the warped markers*: rotation, translation and a stretch along the lead. It also includes shear if `x`/`y` are not orthogonal. Note that `specs/05` §5 still says "rigid"; the reference implementation is affine.

**Trajectory.** With `û = unit(tail − head)`, the start point is `a1 = head − 5·û`. The end point is `a2 = tail + k·contact_span·û`, where `k = 1.5` if the model has **more than 8 levels** (entries of the level index list `etageidx`, not contacts) and `k = 2` otherwise. The trajectory is 50 equally spaced points from `a1` to `a2`. `specs/05` §5 states `N_c > 8`; the reference counts levels, and the two differ only for models with more than 8 contacts and at most 8 levels.

**Native → template ("to MNI").** This step warps the native `coords_mm`, `head` and `tail` point by point. It then rebuilds the template trajectory from the warped head and tail with the rule above (the trajectory is **not** warped) and sets `trajvector`. It builds template `x`/`y` from the **native roll azimuth**, not by warping. The construction is identical to `specs/06` §10.6: take the horizontal part of the native `y − head`, form `x̃ = (ŷ_A, −ŷ_R, 0)`, project it perpendicular to the template lead, and set `y_dir = t × x_dir`. If the brain-shift matrix exists, `scrf` is recomputed first and the warp starts from `scrf`.

**Template → native ("to native").** This step warps `coords_mm`, `head`, `tail` **and the trajectory** point by point. It sets native `x`/`y` with the default frame, so `y` points anterior; **the roll is lost**. It then derives `native` from `scrf` through the inverse brain-shift matrix when that matrix exists.

**L/R check (only inside a hybrid save).** This applies only if both template sides are non-empty and the mean template x of side 1 is less than that of side 2. In that case sides 1 and 2 are swapped in `mni` and `native`, and the file is re-saved. In addition, **only when `props(first side).manually_corrected` is 0**, each template `y` is mirrored through `head` if it lies posterior to `head`, and each `x` if it lies left of `head`. The template contacts are then re-resolved from the markers. Manually corrected files keep the individually warped template contacts. Mirroring only one of `x`/`y` makes the frame left-handed (reference quirk, do not replicate).

## 5. Reader behaviour (what makes a file usable)

### 5.1 The single-subject loader

1. It loads `reco`. If the variable is missing, the loader **fails** (a warning is printed, then the first field access errors).
2. If the requested space is native and `native` is missing while `mni` exists, it runs "to native" and saves. If the template space is requested and `mni` is missing, it runs "to MNI". Both need the subject's Lead-DBS registration transforms (`normalization/transformations/…`, `normalization/log/…_desc-normmethod.json`) and the anchor image under Lead-DBS names. A file exported **without both blocks therefore only works inside a full Lead-DBS subject folder.**
3. The native request returns `scrf` if it exists, unless the caller asked explicitly for native (as manual correction and re-propagation do).
4. If `markers` lacks `x`, it rebuilds `x`/`y` for **both** sides with the default anterior frame. An absent side then errors, so **`x`/`y` are effectively mandatory.**
5. `coords_mm` and `trajectory`, when missing, are rebuilt from the markers (§4). They are optional.
6. `acpc.coords_mm` is returned if present, otherwise NaN.
7. `props(elside).elmodel` and `.manually_corrected` are read. On error, the first non-empty `elmodel` in `props` is used. **`props` missing entirely is a failure.** An empty `manually_corrected` is treated as 0.

**Side detection:** side k is present iff `markers(k).head` is non-empty. `native.markers` is checked if `native` exists, else `mni.markers`.

### 5.2 Consumers

| Consumer | Uses |
|---|---|
| 3D scene, native view | `scrf` > `native`; `coords_mm`, `trajectory`, `markers` as stored |
| 3D scene / VAT, template view | `mni` as stored |
| Lead Group | `mni.markers` + `props.elmodel`. It **re-resolves** `coords_mm` and `trajectory` from the markers with the chosen model, so stored template contacts are ignored. `acpc.coords_mm` is used if present. |
| Lead-Tutor submit | `mni.markers(1)` → `leftElectrode`, `mni.markers(2)` → `rightElectrode`, with fields `head`, `tail`, `x` (matches the project rule "Tutor `leftElectrode` = side 1 = right hemisphere") |
| "Programmer" JSON export | `props(1).elmodel` (so `props(1)` must be non-empty even for a left-only file), `mni.markers`, `native.markers(k).y` for the roll |

### 5.3 Meaning of `manually_corrected`

When the pipeline re-runs reconstruction (unless "overwrite approved" is set), the sides whose `manually_corrected` is truthy are **skipped**. They are skipped both for automatic reconstruction and for the refine (manual) step. The selection concatenates `props(:).manually_corrected` and takes the indices of non-zero entries. **An empty entry shifts the indices** (left-only file with `props(1)` empty and `props(2)` = 1 → side 1 is skipped instead of side 2). Writers must therefore give **every** `props(k)`, k = 1..S, a numeric value.

The flag also changes the manual view (Part 2 §8.1): 0 resets the roll to anterior and snaps the length on first open. It also changes the L/R check (§4).

## 6. Provenance and versioning in Lead-DBS

| What | Where | Content |
|---|---|---|
| Software version | `<dataset>/dataset_description.json` → `LeadDBSVersion` (written when a dataset is created or imported; never refreshed per run) | string read from `<leadroot>/.version.txt` (e.g. `3.2.1`). Compiled builds append ` Standalone`. `Unknown` on failure. `CITATION.cff` carries an older, unrelated `version: 2.5.3` and is not used. |
| Method narrative | `log/sub-<id>_desc-methods.txt` (append-only) | Per step: a timestamp, the name of the calling routine, a free-text method sentence and references. The reconstruction method, for example, is only recorded here. |
| GUI options | `prefs/sub-<id>_desc-uiprefs.mat` | The whole GUI option set, saved as top-level variables (one per option). Overwritten on every GUI change; this is **not** a per-run record. |
| Registration choices | `coregistration/log/…_desc-coregmethod.json`, `normalization/log/…_desc-normmethod.json`, `brainshift/log/…_desc-brainshiftmethod.json` | method + approval |
| Statistics | `sub-<id>_desc-stats.mat` (subject root; variable `ea_stats`: `electrodes` = copy of the electrode structure, `conmat*` contact–atlas distances, `atlases.names/types`, `patname`, `stimulation(i).vat(side,contact)` with amplitude, volume, atlas intersections). Written by the 3D scene and VAT statistics (VAT part saved as v7.3). | derived; regenerated by Lead-DBS |
| Stimulations | `stimulations/<native | MNI152NLin2009bAsym>/<label>/sub-<id>_desc-stimparameters.mat` + VAT/E-field NIfTIs | not related to the reconstruction file |

**No reconstruction options file exists.** Lead-DBS cannot reproduce a reconstruction from its files beyond the method sentence.

## 7. Mapping Reco v2 JSON (`specs/05` §8) → `reco`

| Reco v2 JSON | `.mat` | Rule / gap |
|---|---|---|
| `version`, `subj_id`, `method`, `meta.*` | — | **No place in `reco`.** Do not add fields to `reco`: any Lead-DBS re-save rebuilds `reco` from scratch and drops them. Provenance stays in our JSON. Optional: a second top-level variable (e.g. `dbsloc_provenance`, a struct of char fields: exporter version, source JSON path, `meta.template_space`, `meta.side_keys`, export time). Lead-DBS loads only `reco` by name and ignores it; the few whole-file loads tolerate extra variables. Decision for the caller. |
| side keys `"1"`, `"2"` | index 1, 2 | Same meaning since 2026-10-04. A JSON without `meta.side_keys` is first resolved with `side_hemispheres` (`specs/05` §8). The exporter writes index 1 = right. |
| `props["k"].elmodel` | `props(k).elmodel` (char) | For an absent side **also write the model string** (recommended; avoids the `props(1)` empty pitfalls of §5.2 and §5.3). |
| `props["k"].manually_corrected` | `props(k).manually_corrected` (double) | **Decision needed (recommended):** write 1 if `manually_corrected` is true, **or** `meta.manual_correction[k].review` is `corrected`/`accepted`, **or** the side is directed and `meta.orientation[k].status` ∈ {`resolved`, `ambiguous`, `manual`}. This is what Lead-DBS does itself after its own orientation step, and it stops the Lead-DBS manual view from resetting the roll. Otherwise write 0. Absent side: 0. |
| `native["k"].coords_mm` | `native.coords_mm{k}` | Per-contact rows. If a PaCER side still has per-level rows, re-resolve from the native markers (`specs/05` §6 map `T`) before export (gap). |
| `native["k"].trajectory` | `native.trajectory{k}` | 50×3 as stored. If missing or not 50 rows, synthesise it with §4 *Trajectory*. |
| `native["k"].markers.{head,tail,x,y}` | `native.markers(k).{head,tail,x,y}` | 1×3 rows. Check `|x−head| = |y−head| = r` within 1e-6 (Lead-DBS would rescale them anyway). |
| `native["k"].trajvector` | — | Not a Lead-DBS native field; omit. |
| `mni["k"].*` | `mni.*{k}` / `mni.markers(k)` | `trajvector{k}` included. Our `x`/`y` already follow §4 (same azimuth rule). |
| `mni["k"].coords_mm_warped` | — | Not representable. `mni.coords_mm` = our marker-derived `coords_mm`. This equals Lead-DBS for non-corrected files and is what Lead Group recomputes anyway. |
| `reference` | `mni` (see rule) | Lead-DBS's `mni` must be in **its** template space (default `MNI152NLin2009bAsym`). If `meta.template_space` differs: export `reference` as `mni`, or refuse when there is no `reference` block. |
| `scrf` | `scrf` | We write none (brain-shift is a stub): omit the field. **Never** write `scrf` = `native`, because Lead-DBS would prefer it. |
| `acpc` | `acpc` | Omit (optional; readers return NaN). |
| `meta.orientation[k].roll_deg` | encoded in `y` only | Lead-DBS's displayed rotation = signed horizontal angle from anterior to `y − head`, positive toward −x (Part 2 §8.5). |
| `meta.reconstruct`, `meta.manual_correction` (except as above) | — | Not representable. |

**Unilateral export:** `S` = highest present side index. A right-only file uses `1×1` cells/structs. A left-only file uses `1×2`, with element 1 `[]`, `markers(1)` fields `[]`, and `props(1)` = model string + 0.

**Import (`.mat` → Reco v2):** this is the reverse mapping. Sides are present per §5.1. `manually_corrected` → bool. `meta.side_keys` is written as `"1=right,2=left"` only after the mean-x check passes; otherwise the file is refused. No `meta.orientation` is written (roll unknown) unless the side is directed and `manually_corrected` = 1, in which case `{status: "manual", roll_deg: <from y>}` may be derived (decision for caller). Prefer `native` over `scrf`.

## 8. Acceptance checks — Part 1

1. The exported file name and folder match §1. The file is a v7 MAT whose only variable is `reco` (or `reco` plus the optional provenance variable).
2. Classes and shapes per §3 for a bilateral directional fixture, checked with a MAT reader (no MATLAB): `props` 1×2 struct with char + double, cells 1×2, markers 1×2 struct of 1×3 doubles, `mni.trajvector` present, `native`/`mni` trajectories 50×3.
3. Left-only fixture: `props(1).manually_corrected` is numeric 0 (not empty), `markers(1).head` empty, `coords_mm{1}` empty.
4. Round trip JSON → MAT → JSON reproduces markers, contacts and trajectories within 1e-9 mm and side assignment exactly.
5. `|x − head| = |y − head| = r` within 1e-6 in both blocks. Template `x`/`y` equal the `specs/06` §10.6 construction.
6. Resolving contacts from the exported markers with the §4 affine reproduces exported `coords_mm` within 1e-6 mm in both blocks.
7. Non-reference template space: exported `mni` equals our `reference` block; with no `reference` the export is refused with a clear error.
8. (Phase D, needs a Lead-DBS install.) The single-subject loader returns the stored markers for the native and template requests without triggering a warp. Lead Group lists both leads. A pipeline re-run with "overwrite approved" off skips exactly the sides exported with `manually_corrected` = 1.

---

# PART 2 — The Lead-DBS manual correction view ("Refine localization")

## 9. Entry, space and images

- **Entry.** The view opens after reconstruction when the user ticks "Refine localization". It opens maximised, with the title `<subject>: Electrode Reconstruction`, and starts on the first side that is not approved (§5.3).
- **Edit space.** Always **native, pre-brain-shift** (`native` block, never `scrf`), for CT and MRI alike. An MNI/native toggle exists but is disabled in the reference.
- **Images.**
  - CT: the **raw (preprocessed) post-op CT** in its own voxel grid, with the CT → anchor coregistration affine composed into its header. It is resampled on the fly, so it is not the resliced coregistered CT (preference `reco.mancoruse = 'postop'`; `'rpostop'` would use the resliced coregistered CT).
  - MRI: the **coregistered** post-op MR. The axial pane uses the axial series. The "coronal" plane uses the coronal series, else the axial; the "sagittal" plane uses the sagittal series, else the axial.

## 10. Window layout

The figure has a black background tinted per side: a dark, desaturated version of the side's standard line colour (side 1 dark blue, side 2 dark orange). It is a grid of 3 rows × 6 columns:

| Region | Content |
|---|---|
| Column 1 (all rows) | **Static electrode model diagram**: the catalogue 3D model (insulation + contacts, semi-transparent) seen from the front. The head fiducial is marked with a red star, the tail fiducial with a green star, and the model name is printed. Drawn once. |
| Columns 2–3 (all rows) | **3D "ANTERIOR VIEW"** (camera on +y looking posterior, elevation 0, orthographic) |
| Columns 4–5 (all rows) | **3D "LEFT VIEW"** (camera on −x), a copy of the same objects |
| Column 6, row 1 | **Axial close-up at the head** ("DORSAL VIEW") |
| Column 6, row 2 | **Axial close-up at the tail** |
| Column 6, row 3 | Text box: `Electrode i of n`, `Electrode Spacing: s mm` (2 decimals), `Electrode k/n`, `Rotation: θ deg` |

The window title is `<subject>, Electrode k/n, Electrode Spacing: s mm.`

**3D views (both show the same objects):**

- **Two image planes that contain the lead axis.** The 50-point trajectory is first upsampled 20-fold. At each point the image is sampled trilinearly along a world axis from −10 to +10 mm in 0.2 mm steps:
  - along world **x** for the "coronal-type" plane (axis + x), from the coronal image;
  - along world **y** for the "sagittal-type" plane (axis + y), from the sagittal image.

  Each result is drawn as a texture on the flat quad between the trajectory ends ± 10 mm, at opacity 0.9. In the anterior view the axis-plus-x plane faces the camera and the other plane is edge-on; the left view shows the reverse. The planes always span the whole current trajectory (head − 5 mm to tail + k·span), so **the view re-centres implicitly after every change**: the images are resampled along the new axis.
- **Fiducials.** `head` = red star, `tail` = green star; the selected one turns yellow. Every contact centre (`coords_mm`) is a magenta open circle. The trajectory line is drawn only in verbose mode.
- **Orientation captions** `A`, `P`, `L`, `R` sit 10 mm anterior, posterior, left and right of the head and 10 mm above it.

**Axial close-ups.** A square of ±15 **voxels** of the axial image around the marker. It lies in the image's own voxel plane through the marker (not perpendicular to the lead) and is sampled every 0.5 voxel with higher-order interpolation. A star marks the centre: red for head, green for tail, yellow if selected.

**Intensity.** All images use the same transform with two parameters, *contrast* `c` (default 0.8) and *offset* `o` (default 0.3):
1. Z-score the non-zero samples (statistics from a random sample of up to 15 000) and multiply by `c`.
2. If the image is all non-negative, set its zeros to the minimum.
3. Add `o`, clip to [−3, 3], rescale to [0, 1].
4. Add `o` again, multiply by `c`.
5. Display in grey with a fixed window [0, 1].

A CT threshold guess (800–2800 HU) is computed on first draw but never applied (dead code).

**X-ray mode** (slab averaging):
- Each lead plane becomes the mean of 9 parallel planes at −4…+4 mm (1 mm steps) along the plane normal, displayed with `c + 0.1`, `o − 0.3`.
- The axial close-ups become a Gaussian-smoothed mean of oblique samplings at 31 offsets along the lead (the reference mixes voxel and mm units here; reproduce the intent, a thick-slab view, not the numbers).

## 11. Interaction

### 11.1 On open

The editing session runs on the current side (`elside`). If the file's `manually_corrected` is 0:
- (a) **Length snap:** the tail is moved along the lead so that `|tail − head|` equals the model's canonical head–tail distance.
- (b) **Roll reset:** the rotation starts at 0°, so `y` points anterior. Any automatic roll is discarded.

If `manually_corrected` is 1, the rotation is read from the existing `y` marker (§11.5). **The first redraw saves the file with `manually_corrected` = 1, so opening the view alone marks the reconstruction approved.**

### 11.2 Keyboard map

Keys compare the typed character case-insensitively. Arrow keys use the key name.

| Key | Condition | Effect |
|---|---|---|
| `0` or `4` | — | Select head (distal); press again to deselect. `4` is a legacy alias. |
| `3` or `7` | — | Select tail (proximal); press again to deselect. `7` is a legacy alias. |
| `←` / `→` | marker selected | Move the selected marker by −/+ step along **world x** (anchor mm, not screen axes) |
| `↑` / `↓` | marker selected | Move the selected marker by +/− step along **world y** |
| step for the four above | marker selected | 0.1 mm; **Shift 1 mm; Alt 0.02 mm**. The moved marker's **z is then recomputed so that `|tail − head|` is unchanged**: it pivots about the other marker and stays on the same side (above/below) of it. If the horizontal offset exceeds the length the result is ill-defined (reference uses an absolute value inside the square root). |
| `↑` / `↓` | nothing selected | Move the **whole lead** (head, tail, x, y) along its axis: `↑` proximal, `↓` distal. Step = factor × (stored trajectory length / (M − 1)), factor 0.5, **Shift 3, Alt 0.1**; ≈ 0.27 / 1.6 / 0.05 mm for a 3389 with a 50-point trajectory. `←`/`→` do nothing. |
| `+` / `*` | nothing selected | Lengthen: tail moves proximally by 0.3 × trajectory step (×2 with Shift) |
| `-` / `_` | nothing selected | Shorten: tail moves distally, same steps |
| `<` / `>` | — | Previous / next lead; clears the selection |
| `Space` | — | Not last lead: go to the next lead. Last lead: **finish and save** (§11.6). |
| `h` | — | Show / hide all fiducials, contacts, captions and the text box |
| `x` | — | X-ray mode on / off |
| `c` / `v` | — | Contrast −/+ 0.1 (Shift ×5; minimum 0.1) |
| `b` / `n` | — | Offset −/+ 0.2 (Shift ×5) |
| `m` | — | Numeric dialog (§11.4) |
| `.` | marker selected | "Snap" the marker by the in-plane offset from the close-up centre to the brightest (CT) / darkest (MR) pixel. Marked as not working in the reference. |

**Spacing harmonisation (after every change).** For both leads, the mean spacing between neighbouring contacts or level centres is measured. Then **both** leads' tails are reset so that each lead's `|tail − head|` equals the mean over the two leads. One lead's `+`/`-` therefore changes both leads by half. The saved `coords_mm` are computed *before* this step and can lag the saved markers by one step (reference quirk). The mean spacing is the `Electrode Spacing` readout.

### 11.3 Mouse

There are no custom mouse handlers. Markers cannot be dragged or clicked to select. Only the default MATLAB axes interactions (rotate, zoom, pan from the figure tools) are available.

### 11.4 Toolbar

In order:
1. Previous / next lead.
2. Show / hide fiducials.
3. Contrast −/+, brightness −/+.
4. Select distal contact [0] / proximal contact [3] (toggle buttons).
5. **Detect directional leads (auto mode)**: runs DiODe on the current side, saves its markers with `manually_corrected` = 1, and adopts its roll.
6. **Detect directional leads (manual mode)**: opens the DiODe manual GUI, same adoption.
7. **Rotate lead clockwise** (+15°) / **counter-clockwise** (−15°).
8. Finish (simulates Space).
9. Orientation captions on/off.
10. X-ray toggle.

**`m` dialog** ("Manual electrodes head / tail setting"):
- One editable field per marker (`R0 (Head)`, `R3 (Tail)`, `L0 (Head)`, `L3 (Tail)`), showing `x y z` with 4 decimals, native mm.
- **Update R0, move R3 accordingly**: translates the whole lead so that the head equals the entry.
- **Update R3, move R0 accordingly**: the same with the tail.
- **Update all**: sets every head and tail independently.
- **Cancel**.

The left-side "move accordingly" buttons raise an error in the reference (unhandled action), and the dialog fails for unilateral cases (it references fields of absent sides). Neither should be replicated.

### 11.5 Roll

The roll θ is kept per side in degrees, with 0 = anterior. The roll frame is built as follows:
- the horizontal direction is `(−sin θ, cos θ, 0)`;
- `x̃ = (cos θ, sin θ, 0)` is projected perpendicular to the lead axis `t` and normalised to give `x_dir`;
- `y_dir = t × x_dir`;
- `x = head + r·x_dir` and `y = head + r·y_dir`.

This is the same construction as `specs/06` §10.6. When reading a file, θ is the unsigned angle between anterior and the horizontal projection of `y − head`, made **negative when `y` lies to the +x side of `head`**. Positive θ therefore turns `y` toward −x, the same sign as `roll_from_anterior_deg` for a vertical lead.

The roll is changed only by the ±15° buttons and DiODe; there is **no roll key**. The text box shows `Rotation: θ deg`. Ring leads accept the rotation too (no visible effect). The DiODe buttons on a ring lead show "No Valid Directional Lead Selected!" and change nothing.

### 11.6 Saving

- **Every change is written immediately** (each key, button and redraw), in native space only, with `manually_corrected` = 1 for all handled sides. These intermediate saves rebuild `reco`, so `mni`, `scrf` and `acpc` are **absent** until finish.
- **There is no undo and no discard.** Closing the window keeps the last state; a later template-space load recomputes `mni` on demand (§5.1).
- **Finish** (Space on the last lead or the toolbar) runs these steps in order:
  1. Reload native.
  2. Hybrid save with `manually_corrected` = 1, which recomputes `scrf` (when the transform exists) and `mni` (§4) and `acpc` (when the preference is on).
  3. Run the L/R check (§4; no marker flips because the file is corrected).
  4. Close the window.
  5. Optionally store templates for "auto-improve".
  6. Append a "manually localized… (Horn & Kühn 2015)" paragraph to the methods log.
  7. Continue to 2D/3D visualisation if enabled.

## 12. Gap analysis against `specs/08` §11.1.6 (*Refine localization*)

"Deliberate" = the Wyss spec intentionally differs; keep it unless the product owner decides otherwise.

| # | Lead-DBS behaviour | Current spec | Note / to close |
|---|---|---|---|
| 1 | Edit in native pre-brain-shift space for CT **and** MRI | partial | Wyss edits post-op MRI in template space (`05` §9a). Deliberate (no native block on that path). |
| 2 | CT backdrop = raw CT through the coreg affine | yes (equivalent) | Wyss uses the coregistered CT; same frame, one more interpolation. |
| 3 | Two image planes **containing the lead axis** (axis + world x, axis + world y), ±10 mm, full lead length, auto-following the lead | **missing** | Biggest gap: Wyss shows only voxel-aligned slices, so a tilted lead is never seen end to end. Add two oblique panes resampled along the current axis (0.2 mm), with contacts and handles projected. |
| 4 | Marker-centred axial close-ups of head **and** tail shown together | partial | Wyss has one axial pane through the crosshair. Add two small close-ups (±15 mm) or a head/tail split of the axial pane. |
| 5 | Static model diagram with labelled head/tail fiducials | missing | Low cost; helps users see which contact "head"/"tail" means for > 4-level leads. |
| 6 | Re-centring: images follow the lead after each change | partial | Wyss moves the crosshair to the selected marker; `F` recentres. Adequate with item 3. |
| 7 | Contrast/brightness keys `c`/`v`/`b`/`n` (z-score transform, defaults 0.8 / 0.3) | partial | Wyss: Shift-drag window/level, CT 300/1500 HU. Optional key aliases; the transform need not be copied. |
| 8 | X-ray (thick-slab mean) mode `x` | missing | Add a slab-mean (or MIP) toggle, 9 mm slab, for the lead panes. |
| 9 | Hide fiducials `h` | missing | Add a toggle; `H` is taken by *select head* in Wyss, so choose another key (e.g. `V`, visibility). |
| 10 | A/P/L/R captions with toggle | missing / unspecified | State orientation labels per pane in the spec. |
| 11 | Select head/tail with `0`/`3` (aliases `4`/`7`), toggle deselect | yes (different keys) | Wyss `H`/`T` + click; `0`–`3` are layout keys. Deliberate; document the mapping in F1. |
| 12 | Selected marker: arrows move it along **world x/y** at 0.1 / 1 / 0.02 mm | partial | Wyss moves in **pane axes** at 0.25 / 1 / 0.05 mm. Deliberate (pane-relative is clearer); steps differ. |
| 13 | Selected-marker move **keeps the lead length** (z recomputed, pivot about the other marker) | missing | Wyss moves freely, so length changes. Add a *Keep lead length* option (default off or on, product decision) using the pivot rule in §11.2. |
| 14 | No selection: `↑`/`↓` move the whole lead **along its axis** | partial | Wyss: Alt + keys move the whole lead in the pane plane; there is no along-axis move. Add e.g. Alt+PageUp/PageDown = along the axis, using the Wyss step sizes. |
| 15 | `+`/`-` lengthen/shorten the lead | partial | Achievable by moving the tail. Add explicit keys only if requested. |
| 16 | Length snap to the canonical model length on first open | missing | Deliberate: Wyss never changes geometry the user did not touch. Do not replicate. |
| 17 | Spacing harmonisation across both leads; `Electrode Spacing` readout | missing | Do **not** harmonise (it silently edits the other lead). **Add the spacing readout** (mean adjacent level distance, and the ratio to the catalogue) to the status line. |
| 18 | Roll ±15° buttons; DiODe auto/manual buttons; `Rotation: θ` readout | partial | Wyss has `[`/`]` ±1° / ±10°, slider, field, *Flip 180°*, *Other candidate*, *Automatic* (a superset), but no "re-run orientation detection on this side" action. Optional: a button that runs `specs/06` on the current side. |
| 19 | Roll convention: horizontal azimuth about world z (§11.5) | yes (2026-10-22) | Adopted everywhere: `05` §9a step 2 (manual correction, `--roll`, read-back from the markers), `06` §10.6 `roll_deg` / `candidates_deg` of the orientation stage, and the *Refine localization* view. The former rotation of `a_ref` about the lead axis differed by 0.4° / 1.8° / 4.1° / 7.6° at 10° / 20° / 30° / 40° lateral tilt (roll 45°); files written before the switch hold the axis value in `meta.orientation[k].roll_deg` and are read back from their markers instead. |
| 20 | Roll reset to anterior on opening a not-yet-corrected file | missing | Deliberate: Wyss keeps the file's roll. Do not replicate. |
| 21 | `m` numeric dialog with "set head, move tail accordingly" | partial | Wyss numeric fields set markers independently. Add a *Move whole lead* checkbox next to the fields to cover translate-to-coordinate. |
| 22 | `Space` = next lead / finish and save on last | partial | Wyss `Space` = accept side and go to next; save via `Ctrl+Enter`. Deliberate (explicit save). |
| 23 | `<` / `>` previous / next lead | yes | |
| 24 | Save on every change; no undo, no discard; window close keeps edits | — | Deliberate opposite (undo/redo, explicit save/discard). Do not replicate. |
| 25 | Opening alone sets `manually_corrected` = 1 | — | Deliberate opposite (only edited sides; separate *accept*). |
| 26 | Finish re-derives template block from native (azimuth rule for `x`/`y`) + L/R check | yes | `05` §9a step 4. Wyss has no L/R swap; sides are fixed by hemisphere. Fine. |
| 27 | Methods paragraph appended to the subject log on finish | missing | Add a methods/log line with citation in the subject log when a correction is saved. |
| 28 | Continue to visualisation after finish | yes | Host reloads the 3D scene. |
| 29 | Only the current lead is drawn | superset | Wyss draws the other side dimmed. |
| 30 | Background tint per side | missing | Cosmetic; the side tab label suffices. |
| 31 | Drag markers, zoom/pan, undo, numeric 0.01 mm entry, accept per side | superset | Wyss-only features. |
| 32 | Marker snap to metal `.` (non-functional in reference) | missing | Optional future "snap to metal centre" in the close-up plane. |
| 33 | Roll controls for ring leads | — | Deliberate: Wyss hides them. |

**Suggested order to close:** 3, 13, 14, 17 (readout), 19 (decision), 4, 8, 9, 21, 27, 5, 10.

## 13. Acceptance checks — Part 2 (for the gaps adopted)

1. **Lead panes (item 3):** for an oblique lead (30° tilt) on a synthetic CT with a bright cylinder, both lead panes show the cylinder as a straight band along the pane's long axis through its full length. Contact circles lie on the band within 0.2 mm. After a head nudge the panes are resampled along the new axis within one frame.
2. **Keep lead length (item 13):** with the option on, 20 random nudges of the head keep `|tail − head|` within 1e-9 mm. The tail is unchanged, and the head's in-plane coordinates equal the requested ones.
3. **Along-axis move (item 14):** one step moves head and tail by the same vector parallel to `tail − head`, of the configured length within 1e-9 mm. The roll is unchanged within 1e-6°.
4. **Spacing readout (item 17):** for a catalogue-length lead the readout equals the catalogue spacing within 0.01 mm. Stretching the lead by 10 % raises it by 10 %. The other side's markers are never changed by an edit of this side.
5. **Roll convention (item 19):** after the decision, for a vertical lead the panel roll, `roll_from_anterior_deg` and the Lead-DBS formula (§11.5, read-back rule) agree within 1e-6°. For a 30° lateral tilt at roll 45° the documented convention is the one the pipeline writes (difference to the other convention 4.1° ± 0.1°).
6. **Slab mode (item 8):** toggling it changes only the display. The saved markers are byte-identical.
7. **Hide fiducials (item 9):** the toggle hides handles, contacts and axis lines in every pane. Editing keys still act on the selected marker.
8. **Methods log (item 27):** saving a correction appends exactly one dated paragraph naming the corrected sides to the subject log.

---

**Uncertainties.**
- The exact class and shape of `acpc.ac` / `pc` / `msp`.
- Whether any Lead-DBS consumer reads `mni.trajvector` (none found).
- The step-size claims in §11.2 that depend on the stored trajectory being the 50-point construction.

All other statements were checked directly against the reference behaviour.
