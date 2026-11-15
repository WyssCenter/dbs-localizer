# Spec 05 — Electrode Reconstruction (Wyss DBS Localizer)

**Status:** Implemented: the `reconstruct` stage (TRAC/CORE on the coregistered post-op CT and on the normalized post-op MRI, PaCER on CT as an optional build component, import of a pre-exported PaCER result), the Reco v2 reconstruction file with its Lead-DBS `.mat` twin, and manual correction; nothing further is planned in this specification.
**Depends on:** `00` (stage contract §5.2, locks §6), `01` (subject record, post-op modality §10, derivative names §7a), `03` (CT coregistration), `04` (normalization, template bridge §5.4a), `06` (orientation, template `x` / `y` construction §10.6), `09` (electrode catalogue, geometry rules §2.2), `10` (PaCER algorithm), `12` (`.mat` format, reference correction view).

---

## 1. Purpose and terms

The `reconstruct` stage localizes the DBS leads of one subject and writes their contacts, trajectories and fiducial markers in native and template space. One stage implementation (`crates/dbsloc-pipeline/src/stage_impl.rs`, `fn run_reconstruct`) serves the GUI, `dbsloc run`, `dbsloc_pipeline_runner` and the server. The algorithms, without file or process I/O, live in `crates/dbsloc-reconstruct` (TRAC/CORE, native frame, metal axis, plausibility, tail-fiducial conversion) and `crates/dbsloc-pacer` (PaCER).

| Method | Post-op image | Traced in | Section |
|---|---|---|---|
| TRAC/CORE, native frame, with metal-axis fallback | CT | coregistered CT, native mm | §4.4 |
| TRAC/CORE, template space | MRI | normalized post-op MR, template mm | §4.1–§4.3 |
| PaCER | CT | coregistered CT, native mm | §7 |
| PaCER import | CT | pre-exported file | §3.4 |
| Manual correction | either | the file's edit space | §9a |

Terms:

- **Spaces**: *native* is the anchor-native space of the coregistered images (`03`, the `native` block); *template* is the space of the normalization template (`04` §3.1, `meta.template_space`), whose block keeps the Lead-DBS name `mni` whatever the template; the *reference space* is `MNI152NLin2009bAsym` (`04` §5.4a, the `reference` block).
- **Level**: one axial contact position, a ring or the three segments of a directional level. **Contact**: one element; a directional level has three.
- **Markers** (fiducials): `head` = centre of the most distal level; `tail` = the model tail fiducial of `09` §2.2; `x`, `y` = points at radius `r = lead_diameter / 2` from `head`, perpendicular to the lead. `fiducial_span` = the expected `|tail − head|` (`09` §2.2).
- **Side keys**: `"1"` = right hemisphere, `"2"` = left (§8.3).

---

## 2. Side geometry

Every coordinate block (`native`, `mni`, `reference`, `scrf`) maps side keys to objects with these fields, in RAS world millimetres of the block's space:

| Field | Content |
|---|---|
| `coords_mm` | `N × 3` contact centres in catalogue order; row 0 is the most distal contact (`09` §3). PaCER's native block has one row per level instead (§7). |
| `trajectory` | 50 × 3 points along the lead (§5). |
| `markers` | `head`, `tail`, `x`, `y`, each a 3-vector. |
| `trajvector` | unit vector `head → tail` (distal to proximal). |
| `coords_mm_warped` | template-space blocks only: the individually mapped contacts (§6.3). |

---

## 3. Dispatch

### 3.1 Prerequisites and electrode model

Before the stage body runs, the driver requires a normalization folder with at least one normalized post-op image (`crates/dbsloc-pipeline/src/prereqs.rs`); otherwise the stage fails with `reconstruct: no normalization/anat/ — run normalize with post-op data so template-space NIfTIs are written` or `reconstruct: needs a normalized post-op image (CT or MR, listed by normalization/log/*_desc-normmethod.json) under normalization/anat/ — only pre-op template anatomy is not enough for TRAC/CORE on post-op`.

**Electrode model** (`stage_impl.rs`, `fn opts_with_prefs_model`, `fn resolve_electrode`). A non-blank `RunOptions.el_model` (`--el-model`, GUI picker) wins. Otherwise the `elmodel` variable of `prefs/sub-<id>_desc-uiprefs.mat` is used (`crates/dbsloc-pipeline/src/uiprefs.rs`) and logged as `dbsloc: note: reconstruct: electrode model from <uiprefs path>: <model>`. A missing file or an absent or empty `elmodel` variable counts as no model; an unreadable file logs `dbsloc: warning: reconstruct: cannot read the electrode model from <error>` and counts as absent. There is no default lead. Without a model the stage fails with ``no electrode model set; pass el_model in RunOptions (CLI `--el-model '<name>'`, GUI reconstruct panel) or set elmodel in the subject's prefs/sub-<id>_desc-uiprefs.mat`` (or with `RunOptions.el_model_error` when the front end set one). A name the catalogue does not know fails with `E_RECON_elmodel: "<name>" is not in the electrode catalogue (<n> known)`. Names match the catalogue display name or file name, ignoring case and surrounding white space (`09`).

### 3.2 `recon_method`

`RunOptions.recon_method` (default `auto`) is trimmed and lower-cased (`crates/dbsloc-pipeline/src/ct_recon.rs`, `CtMethod::parse`). Accepted values: `auto` (alias: the empty string), `trac_core` (aliases `trac`, `core`), `trac_core_native`, `pacer`. Front ends send only canonical names: `dbsloc run --recon-method` accepts the four names (any other value is a usage error, exit 2), the GUI sends its choice's canonical name, and the server refuses any other value with HTTP 400 `unknown recon_method "<value>" (known: <names>)` (`crates/dbsloc-server/src/runs.rs`; `pacer` is not listed in a build without the feature). Aliases reach the stage only through `RunOptions` JSON (`dbsloc_pipeline_runner`) and the research binding `dbsloc_reconstruct.reconstruct_ct` (`crates/dbsloc-pipeline/src/python.rs`). The binding raises `ValueError` with the unknown-method message below, quoting the value as given (not trimmed or lower-cased).

The post-op modality is `Subject.postop_modality` (`01` §10 step 5: CT when a post-op CT exists, unless the modality choice or the subject's `uiprefs.mat` selects MRI).

| Post-op modality | `recon_method` | Result |
|---|---|---|
| CT | `auto`, `pacer` | a pre-exported PaCER result (§3.4) when one exists |
| CT | `auto`, `trac_core`, `trac_core_native` | native-frame TRAC/CORE (§4.4) |
| CT | `pacer` | PaCER (§7) |
| MRI or none | `auto`, `trac_core` | template-space TRAC/CORE (§4.1–§4.3) |
| MRI | `pacer` | `E_PACER_mri: PaCER is CT-only (spec 05).` |
| MRI | `trac_core_native` | `E_RECON: recon_method=<m> requires post-op CT (spec 05); the native path traces the coregistered CT` |
| none | `pacer`, `trac_core_native` | the same `E_RECON: recon_method=<m> requires post-op CT …` |

Order of checks in `fn run_reconstruct`: `pacer` in a build without the `pacer` feature fails with `E_RECON_pacer_unavailable: …` (§7); then an unknown value fails with `E_RECON: unknown recon_method "<value>" (auto, trac_core, trac_core_native, pacer)`, `<value>` trimmed and lower-cased, before the post-op modality is looked at (owner decision 2026-11-14); then the modality branch of the table applies. Sides reviewed in a manual correction are then selected for keeping (§9a *Reconstruct re-run*); when every side is kept, nothing is traced and the file is rewritten from the existing one. On a CT the PaCER import (§3.4) is checked before the blockers of §3.3, and both before the electrode model is resolved.

Failures of the method checks and of the tracing itself carry the prefix `recon:` (`fn recon_err`, `00` §5.2), for example `recon:E_RECON: unknown recon_method "x" (auto, trac_core, trac_core_native, pacer)`. Three failures reach the run without it: the TRAC-window bridge error (§4.3), a failure while carrying the CT brain masks for PaCER (§7), and an error while writing the file (§9).

### 3.3 CT blockers

On a post-op CT nothing falls back to the template path. Before the electrode model is resolved, missing inputs fail the stage with `E_RECON_native: <what>` (`ct_recon.rs`, `fn native_blocker`), `<what>` being `no template given (--template), so points cannot be warped to it`, `template <path> does not exist, so points cannot be warped to it`, `no coregistered post-op CT (run coreg first)` or `no native->template transform from the normalize stage` (also when the normalization was made for another template space than the run's template, `stage_impl.rs`, `fn normalization_matches_template`). An input path with a `..` component fails with `E_RECON_input: path <path> contains a '..' component`.

### 3.4 PaCER import

For `auto` and `pacer` on a CT, a pre-exported Reco v2 file replaces tracing when it exists: the file named by `$DBSLOC_PACER_RECO_JSON` (trimmed, non-empty, an existing file), else `reconstruction/sub-<id>_desc-pacer_import.json`. Reading it needs no PaCER code, so `auto` honours it in every build; `pacer` without the feature fails before the import is read (§3.2). `trac_core` and `trac_core_native` ignore it. Validation (`ct_recon.rs`, `fn pacer_import`): unparseable JSON fails with `<path>: <error>`, a `version` other than 2 with `RecoFileV2: expected version 2`. Older tail markers are converted (§8.8), the warnings logged prefixed with `<path>: `. `subj_id`, with or without `sub-`, must equal the subject (`RecoFileV2 <path>: the import is for subject "<id>", not "<id>"`); a file named by the environment variable must carry it (`RecoFileV2 <path>: $DBSLOC_PACER_RECO_JSON names a file without subj_id, so it cannot be matched to subject "<id>"`), the per-subject file may omit it. `mni` must be a non-empty object (`RecoFileV2 <path>: missing or empty mni block`) and `native` must hold at least one side object (`RecoFileV2 <path>: no native block with a lead side; a post-op CT import needs native coordinates`). Per-side problems of `mni` are not errors; they are listed in `meta.pacer_import_warnings` (`side R (1): expected object, got <type>`, `side R (1): markers.head/tail required`, `L (2)` for the left side). The file is otherwise taken as it is: an empty `method` becomes `pacer_import`, `meta.pacer = "import_json"` is set, and the plausibility gates do not run. The stage then adds what every write adds (§9).

---

## 4. TRAC/CORE

TRAC traces the lead's centreline through axial slices; CORE locates the lead's metal along that line and places the markers from the electrode model. Both are an independent implementation in `crates/dbsloc-reconstruct/src/trac.rs` (`fn trace_side`, `fn core_on_line`). CORE does not use Lead-DBS's electrode template images: its metal model comes from the catalogue (`09`).

### 4.1 Input volume

- **Template path, post-op MRI** (`stage_impl.rs`, `fn reconstruct_template`). The volume is the normalized post-op MR in use: the first of `ax_MRI`, `cor_MRI`, `sag_MRI` with a normalized image (`Subject::norm_postop_mr_in_use`). Only when none exists are all normalized post-op images averaged with equal weights (`composite_mean_3d`), each image whose file name contains `CT` negated; they must share one grid, else `NIfTI grid mismatch: <file> vs first volume (shape/affine differ)`. No bone mask is applied. The images count only when the normalization matches the run's template (§3.3); without one the stage fails with `E_RECON_input: no normalized post-op NIfTIs; run normalization or pass paths`. The lead is traced as a dark artefact unless `RunOptions.recon_bright_metal` (`--recon-bright-metal`, GUI metal contrast *Bright*) is set.
- **Native path, post-op CT** (§4.4). The coregistered CT as loaded, negated so the metal reads dark, whatever `recon_bright_metal` says. When that option is set the run logs `dbsloc: note: reconstruct: the bright metal setting does not apply to a post-op CT in HU; the CT is traced with its metal bright as scanned`.

### 4.2 Per-side trace

TRAC, CORE and §5 run once per hemisphere. TRAC's own hemisphere index is 1 = left (reference-space *x* < 0) and 2 = right. It appears in messages (`E_RECON_trace: hemisphere <h>: …`) and is not a side key.

**TRAC** works on axial slices (the stored axis most parallel to world *z*) from the start plane toward the end plane (§4.3). It seeds on the start slice, averaged over the seed slab: the most metal-like compact component (at most `seed_max_area_mm2`) of this hemisphere whose centroid lies in the seed ellipse. It then walks slice by slice, taking a damped centroid of the thresholded artefact inside a search window that follows a robust line fit, accepting points within the plausibility radius of the prediction; `end_tolerance` consecutive misses end a pass, and `refine_steps` further passes follow the previous line. Output: the fitted centreline, deepest first. Errors: `start/end z window does not intersect the volume; check the template space` (fewer than 4 slices or slice steps between the planes), `no electrode artifact in the start plane for this hemisphere; if the volume has bright un-inverted metal, metal_dark is the wrong way round`, `too few accepted slices to fit a line` (fewer than 4 points).

**CORE** samples, along the centreline extended 6 mm deeper, the most metal-like value on a transverse disc of radius `max(profile_radius_mm, r)`, and finds the lead's metal as the first half-maximum region, insulator gaps closed. Its model extent runs from the distal edge of the most distal level's metal (the tip apex when the tip is a contact) to the proximal edge of the last level (`09`). Errors: `no metal region of plausible length along the traced line (CORE)`; `metal extends only <m> mm along the traced line, too short for the <e> mm this electrode should show (CORE)` (shorter than the model extent minus max(25 %, 2 mm)); `traced centreline is shorter than the electrode metal extent`.

**Placement (bloom rule).** `head` is the half-maximum crossing at the region's distal edge, moved up the line by the model's distance from the distal metal edge to the most distal level centre plus `distal_bloom_mm` (0.5 mm; phantoms painted without blooming use 0). `tail = head + fiducial_span · û` along the line. `x`, `y` are an arbitrary perpendicular pair at radius `r` (`crates/dbsloc-reconstruct/src/ring.rs`, `fn ring_in_plane`). The markers then pass through §5. *Why the bloom:* the disc maximum makes blooming move the visible edge beyond the contact; 0.5 mm fits both a CT whose contacts resolve as separate peaks (true bloom 0.0–0.6 mm) and the Lead-Tutor ten-subject mean error per lead (1.651 / 1.526 / 1.499 / 1.489 mm at 1.0 / 0.65 / 0.5 / 0.35 mm).

`fn core_on_line` runs CORE alone on a supplied line from `deep` to `shallow` (§4.4); the line must be at least the lead's apex-to-proximal-metal length (`09` §3.1), else `traced centreline is shorter than the electrode metal extent`. A model with a single level is refused with `<model> has a single contact level, so TRAC/CORE cannot place a head and a distinct tail on it`; PaCER refuses it as well (fewer than two levels fails with `EmptySkeleton`, `10` §8a step 4).

### 4.3 Parameters and reference-space windows

The parameters are `TracConfig::default()` (`trac.rs`). No command-line flag changes them; the research binding can override `distal_bloom_mm`.

| Field | Default | Meaning |
|---|---|---|
| `start_z_mm`, `end_z_mm` | 8.7, −15.5 | start and end plane, reference-space *z* |
| `seed_abs_x_mm`, `seed_abs_y_mm`, `seed_max_area_mm2`, `seed_slab_mm` | 45, 60, 40, 5.0 | seed ellipse semi-axes (reference-space *x*, *y*), largest seed component (mm²), seed slab depth |
| `refine_steps`, `mask_window_mm`, `tra_std_factor`, `plausibility_radius_mm`, `end_tolerance` | 1, 5.0, 3.0, 7.5, 10 | walk: extra passes, search-window half-width, threshold factor, accept radius, consecutive misses that end a pass |
| `profile_radius_mm`, `metal_extent_tol_frac`, `metal_extent_tol_min_mm`, `distal_bloom_mm` | 1.5, 0.25, 2.0, 0.5 | CORE: disc radius (at least `r`), allowed metal-extent shortfall, bloom (§4.2) |
| `world_to_reference` | `None` | below |

The seed slab is `max(1, round(seed_slab_mm / slice spacing))` slices, so the seed sees the same stretch of lead on any slice spacing.

**Reference-space windows.** The start and end planes, the seed ellipse and the native-frame brain box of §4.4 are millimetres of the reference space `MNI152NLin2009bAsym` (`04` §5.4a). `world_to_reference` is the row-major 4 × 4 map `M` from the volume's world millimetres to reference millimetres; `None` means the volume is already in that space. When it is set, the seed-ellipse test and the hemisphere split use `M · world`, and a plane is the slice at which the reference-space line `(0, 0, z)`, mapped through `M⁻¹`, crosses the grid. Window size, radii, areas and the CORE profile stay in the volume's own millimetres. A singular `M`, or a slice axis that does not move along reference *z*, fails with `affine is singular`.

The stage sets `M = Bridge::affine_to_reference` whenever the run's template space is not the reference space, logging `[reconstruct] TRAC windows mapped from MNI152NLin2009bAsym through the <space> bridge`. It applies to the template path and, on frame coordinates, to the native path (§4.4). A bridge that cannot be built fails the stage with `reconstruct: template space <space> needs the MNI152NLin2009bAsym bridge for the TRAC search windows: <error>`; with `DBSLOC_NO_BRIDGE=1` there is no bridge and the windows are used unchanged.

### 4.4 Native-frame CT path

`ct_recon.rs` (`fn native_frame_reco`), `crates/dbsloc-reconstruct/src/native_frame.rs` (`fn trace_native_sides`) and `metal_axis.rs`. The coregistered CT is traced on its own grid under a rigid, template-like frame; only the resulting markers are warped to the template.

**Frame.** A 3 × 3 × 3 lattice spanning the central 50 % of the CT's world bounding box (between the corner voxel centres) is sent through the point warp (§6.1). The frame is the least-squares rigid fit (Kabsch) of lattice → warped lattice. `scale = sqrt(Σ|warped − mean|² / Σ|lattice − mean|²)`; `frame_fit_rms_mm` is the RMS residual of the similarity fit (scale · rotation + translation). TRAC/CORE runs on the unresampled CT with voxel-to-world `frame · affine`. The start and end planes and the seed semi-axes are divided by `scale`, and `world_to_reference` applies to frame coordinates. The markers return to native space through the inverse rigid map and are resolved there (§5); the native block is then warped (§6).

Per side:

1. **TRAC/CORE with axis refinement.** After TRAC/CORE succeeds, the metal-axis refinement of step 2 runs on all CT voxels ≥ 1800 HU (no hemisphere box; its cylinders select the lead), starting 5 mm distal of the TRAC `head` along `head → tail`, and CORE re-runs on the refined axis with the step-2 window. Its markers replace the TRAC/CORE ones; if the refinement or that CORE fails, the TRAC/CORE markers stand. The side's source is `trac_core`. *Why:* TRAC's direction is a robust mean of centroid steps, so a 0.35° change of the CT coregistration moved one traced head by 1.16 mm; the refinement lowered the per-subject error SD under small coregistration perturbations from 0.21–0.22 mm to 0.08–0.15 mm.
2. **Metal axis**, evaluated only when TRAC/CORE fails. The voxels ≥ 1800 HU inside the hemisphere's brain box (reference-space |x| < 45, −60 < y < 45, −25 < z < 70 mm, each bound divided by `scale`; left = *x* < 0) are fitted with a RANSAC line (at least 30 voxels, inlier radius 1.5 mm, fixed seed; else `fewer than 30 voxels at or above 1800 HU in the hemisphere`), the line is refined by intensity-weighted principal axes in cylinders of radius 3, 2 and 1.5 mm over −10 … +30 mm from its deep end (else `metal-axis refinement lost the RANSAC line`), and CORE runs on it from 4 mm below the axis's deep end to the lead's apex-to-proximal-metal length plus 10 mm above it. **Continuity** is the fraction of 0.5 mm samples over the 30 mm proximal of CORE's `head` whose transverse disc maximum (radius 1.5 mm) is ≥ 1800 HU. *Why 1800 HU:* the lead's contacts and body read 2000–3070 HU along its axis on the Lead-Tutor CTs, while the 99.9th percentile of each whole volume (skull included) is 1450–1560 HU; leads that a misaligned coregistration made look steep read continuity 0.95–1.00.
3. **Selection.** TRAC/CORE when it traced the side; else the metal axis when it is corroborated by continuity ≥ 0.9 (else `only <p>% of the 30 mm above contact 0 reads as lead metal (>= 1800 HU; need 90%)`); else the side fails with `<left|right>: TRAC/CORE: <error>; metal axis: <error>` or `<left|right>: TRAC/CORE: <error>; metal axis not corroborated: <why>`. `meta.side_source[<left|right>]` is `trac_core` or `metal_axis`. A metal-axis side also gets `meta.metal_axis[<left|right>] = {continuity, continuity_mm: 30, threshold_hu: 1800, seeds: {ransac_voxels}}` and `meta.side_notes[<left|right>] = "TRAC/CORE could not trace this side (<error>)"`.

Continuity and the metal-on-axis gate (§4.5) sample the CT every 0.5 mm along the axis, on the axis and as a transverse disc maximum (`sampling.rs`, `fn axis_and_disc_samples_in_bounds`). A sample outside the grid gives no value: continuity counts that depth as not metal, and the gate fails the side (`non-finite image samples along the axis`).

**One lead (normative, 2026-10-16).** When exactly one side cannot be traced (step 3), the stage writes a unilateral reconstruction. The failed side has no `native`, `mni`, `reference` or `props` entry and no `side_source`, `metal_axis` or `side_notes` entry. The run logs `dbsloc: warning: reconstruct: the <left|right> lead (side <k>) was not reconstructed and is left out of the file: <reason>`, and `meta.reconstruct` records `status[<k>] = "failed"` and `failed[<k>] = <reason>` (§8.6). When both sides fail, the stage fails with both reasons joined by ` | `. A traced side that fails a plausibility gate (§4.5) fails the stage; it is not dropped, because a gate failure points at the coregistration or the trace, not at an absent lead. Readers accept one side (§8.3); Lead-Tutor (`crates/dbsloc-pipeline/src/tutor.rs`) refuses a unilateral file. Sides kept from a manual review (§9a) are not traced; a failing traced side next to a kept side is left out in the same way, and the stage fails only when no side is traced or kept. On the template path a side is left out only next to a kept side; without kept sides a trace failure fails the stage with `E_RECON_trace: hemisphere <h>: <error>`.

### 4.5 Plausibility gates

`crates/dbsloc-reconstruct/src/plausibility.rs`. Every written side must pass the gates of its path. Any failure fails the stage with `E_RECON_implausible: refusing to write <n> problem(s): <problem> | <problem> …`; each problem is prefixed with `<left|right> (native): `, `<left|right> (mni): ` or, on the template path, `<left|right>: `.

| Gate | Rule | Native CT | Template MRI | PaCER |
|---|---|---|---|---|
| span | the head–tail distance lies within `max(0.25 · fiducial_span, 2 mm)` of `fiducial_span` | native | template | native |
| pitch | adjacent level centroids within `max(0.2 · smallest model pitch, 0.3 mm)` of the model pitch | native | template | native |
| shaft radius | every contact within `r + max(0.25 · r, 0.3 mm)` of the head–tail axis | native | template | native |
| field of view | every contact, `head` and `tail` inside the world bounding box of the traced volume | native | template | native |
| metal on axis | over `head → tail`: median on-axis value ≥ 50 % of the median disc maximum, and ≥ 300 HU | native | — | — |
| tilt | angle between `tail − head` and the *z* axis ≤ 55° | template | template | template |
| brain mask | for every template contact, the voxel of `<template folder>/brainmask.nii.gz` nearest to it is set, or some set voxel centre lies within a Euclidean distance of 3 mm (world mm, not a per-axis box); skipped when the file is absent | template | — | template |

The problem texts:

- span: `head→tail span <s> mm deviates <d> mm from model span <e> mm (tol <t>); head/tail are probably centreline extremes, not the head / tail fiducial level centres`
- pitch: `level <i> → <j> pitch <p> mm deviates <d> mm from model pitch <e> mm (tol <t>)`
- shaft radius: `contact <i> sits <d> mm off the shaft axis, beyond the lead radius <r> mm (tol <t>)`
- field of view: `contact <i> at [x, y, z] is outside the traced volume ([min] … [max])`, likewise `head at …` and `tail at …`
- metal on axis: `median on-axis value <a> is below 50% of the disc maximum <d> over the contact span; the axis runs beside the bright structure the trace followed, not through a lead`; `median on-axis value <a> is below 300; nothing metallic sits on the traced axis`
- tilt: `trajectory tilts <t>° from vertical, beyond 55°; a lead from a frontal burr hole to a deep target does not lie this flat, so either the trace started on something other than the lead or the CT sits rotated against the anchor — check the CT→anchor coregistration`
- brain mask: `contact <i> at [x, y, z] lies outside the template brain mask (margin 3 mm); a DBS contact is inside the brain, so the trace followed bone or soft tissue, not the lead`

**No tilt override.** The 55° gate applies to every side, whichever path produced it, metal continuity notwithstanding: continuity rules out a trace that started on bone, not a CT left rotated by its coregistration. Older files may carry `meta.tilt_gate` from a removed override; nothing reads it.

---

## 5. Markers → contacts and trajectory

`native_frame.rs`, `fn resolve_fiducials(fids, electrode)`; the trajectory is `ElectrodeGeometry::trajectory` (`crates/dbsloc-reconstruct/src/electrode_catalog.rs`).

Input: `[head, tail, x_hint, y]` (the fourth point is ignored) and the catalogue model. Output: `coords_mm`, `trajectory`, `markers`, `trajvector`.

1. With `û = unit(tail − head)`, `x` and `y` are rebuilt at `head`: `x = head + r · unit(component of x_hint − head perpendicular to û)` (an arbitrary perpendicular when the hint is parallel to the lead), `y = head + r · (û × x̂)`.
2. A least-squares **rigid** transform (Kabsch, no scale) maps the four model fiducials (`09` §2.2: head and tail on the model axis at the head and tail level depths, `x` / `y` at radius `r` along model +x / +y at the head depth) onto `[head, tail, x, y]`. `coords_mm` is that transform applied to every catalogue contact centre, in catalogue order. When `|tail − head|` equals `fiducial_span` (TRAC/CORE, metal axis) the fit is exact; otherwise the misfit is spread over the fiducials and the contacts keep the model spacing. Lead-DBS solves an exact affine here (`12` §4); template-space blocks get that map in §6.3. The writers that use this rigid fit are `reconstruct` and the manual correction (§9a step 3); the `orientation` stage instead rebuilds the native contacts from the markers with the stretched map of §6.3 (`orientation_stage.rs`, `contacts_from_markers`; `06` §10.6(a)), so the two contact sets differ when `|tail − head|` differs from `fiducial_span`.
3. `trajvector = û`; `trajectory` follows the rule below. `head == tail` fails with `head and tail coincide`.

**Trajectory (normative, 2026-11-14).**

```
û  = (tail − head) / |tail − head|
a1 = head − 5 mm · û
a2 = tail + k · contact_span · û
trajectory = 50 points equally spaced from a1 to a2
```

`contact_span` (the catalogue array span) and the extension factor `k` are defined in `09` §2.2, which owns these geometry rules. Every trajectory written since 2026-11-14 follows this construction: native blocks, template-space blocks (from the block's own `head` and `tail`, §6.3), the `reference` block, PaCER's native block, manual correction and the exports of `12` Part 1. A stored trajectory of an older file is rebuilt only when that side is converted (§8.8) or rewritten.

---

## 6. Spaces and point warps

### 6.1 Native → template point warp

`ct_recon.rs` (`fn point_warper`, `fn warp_sides`) and `crates/dbsloc-pipeline/src/point_warp.rs`.

The transform is the normalization's inverse composite `normalization/transformations/sub-<id>_from-<Space>_to-anchorNative_mode-image_xfm.h5` (`01` §7a), which maps points from native to template space; it is used only when the normalization matches the run's template (§3.3). The route (`stage_impl.rs`, `fn warp_route_for`):

| Condition | Route |
|---|---|
| normalize backend `ants` (`--reg-backend`, `--normalize-backend`) | `antsApplyTransforms` on a coordinate image |
| the normalization record says `reg_backend: "wgpu"` | `fastwarp-reg` in process; an unreadable composite fails with `E_RECON_warp: native to template: <error>` |
| otherwise | `fastwarp-reg` when it can read the composite, else `antsApplyTransforms` with `dbsloc: note: reconstruct: InverseComposite.h5 is not readable by fastwarp-reg (<error>); the point warp uses antsApplyTransforms` |

**Coordinate image.** Only `antsApplyTransforms` is bundled, so points are mapped as an image: a float32 4-D image over the template's world bounds ± 120 mm on an 8 mm grid, whose voxel values are their own RAS coordinates (sform code 2, qform code 0), is resampled with `antsApplyTransforms -d 3 -e 3 -n Linear -t <composite>` onto a 1 mm grid laid around the points (± 10 mm, origin floored to the grid) and sampled trilinearly at the points. ITK threads follow `RunOptions.ants_max_threads`. A point whose three mapped components are all 0 lies outside the coordinate image and fails the warp: `<n> point(s) mapped outside the coordinate image (<lo> to <hi>); the transform does not fit these points and this reference`. The `fastwarp-reg` route (`fastwarp_reg::map_points_ras_lattice`) evaluates the same lattice with the same float32 storage and trilinear blend without a process.

All points of a reconstruction (contacts, trajectories and markers of both sides) go through one call. Then, per side, `x` / `y` are replaced as in §6.2, `trajvector` is recomputed from the warped `head` / `tail`, and §6.3 runs. Failures are `E_RECON_warp: frame fit: <kind>: <error>` (the lattice warp of the §4.4 frame), `E_RECON_warp: frame fit: <error>` (the rigid fit of that frame, `fn fit_frame`) and `E_RECON_warp: skipped: <kind>: <error>` (the warp of the native block, `fn warp_to_template`); `<kind>` is `ValueError`, `FileNotFoundError` or `RuntimeError`.

### 6.2 Template `x` / `y`

Template `x` / `y` are not the warped points: they are built from the native roll azimuth and the warped `head` / `tail` by the construction of `06` §10.6, at the radius of the native `|y − head|` (`crates/dbsloc-pipeline/src/template_contacts.rs`, `fn construct_template_markers`). When the native `y − head` has no horizontal part, the individually warped `x` / `y` are kept, the run logs `dbsloc: warning: reconstruct: mni side <k>: vertical_marker: the native segment direction has no horizontal part; template x / y kept as individually mapped points`, and `meta.reconstruct.flags[<k>]` gets `vertical_marker` (§8.6).

### 6.3 Template-space contacts from the markers (normative, 2026-10-15)

After every write of a template-space side block (the `mni` block of the point warp, of PaCER and of the template path, a manual-correction re-warp (§9a), an orientation rewrite of `x` / `y` (`06` §10.6), and the `reference` block (§8.5)), its `coords_mm` and `trajectory` are recomputed from that block's markers and the electrode model (`template_contacts.rs`, `fn recompute_template_contacts`; `native_frame.rs`, `fn contacts_from_markers`):

- `T(p) = head + [x̂ ŷ û] · diag(1, 1, s) · (p − head_model)`, with `x̂ = (x − head)/r`, `ŷ = (y − head)/r`, `û = unit(tail − head)`, `s = |tail − head| / fiducial_span` (`s = 1` when the model span is 0 or not finite). `T` maps the four model fiducials exactly onto the markers: rotation, translation and a stretch along the lead.
- `coords_mm` = `T` of every catalogue contact centre, in catalogue order (one row per contact, segments included), so the contacts lie on the straight body that `08` §11.1.2 draws.
- `trajectory` = §5 from the block's `head` and `tail`. `markers` and `trajvector` are unchanged.
- `coords_mm_warped` holds the individually mapped contacts, rows as in the source block. It is written whenever a warp or the bridge has just mapped them (point warp, PaCER, native-space correction, `reference` block); omitted for blocks written directly in template space (template path, template-space correction); kept for a side that is not rebuilt, and taken once from the old `coords_mm` of a file that predates this rule. An orientation rewrite re-warps the side's rebuilt native contacts into `coords_mm_warped`, or drops it with the warning of `06` §10.6 when no warp is available.
- Degenerate input keeps the previous `coords_mm` and logs `dbsloc: warning: reconstruct: <block> side <k>: contacts kept as individually mapped points (<why>)`, never an error. `<why>` is `no electrode model`, `electrode model "<name>" not in the catalogue`, `a marker is missing` or `markers are degenerate` (a non-finite value; `|tail − head|` ≤ 1e-6 mm; `|x̂ · û|`, `|ŷ · û|` or the cosine between `x̂` and `ŷ` above 0.25; `|x − head|/r` or `|y − head|/r` outside [0.5, 2.0]). The model is `props[<k>].elmodel`, else `meta.elmodel`, else the run's model.
- The native block is never changed by this step; only an orientation rewrite and a manual correction recompute native contacts. PaCER's native block keeps one row per level, while its template block has one row per contact.
- `meta.template_contacts = "from_markers"` marks a file in which a block was recomputed. Files without it predate the rule and keep their individually warped template contacts until a writer of this section rewrites them.
- Deliberate deviation from Lead-DBS: this applies to every file, manually corrected or not (Lead-DBS keeps individually warped contacts for manually corrected files). Effect on the Lead-Tutor cohort: at most 0.17 mm along and 0.23 mm across the lead; `head` / `tail`, and so Lead-Tutor scores, are unchanged.

---

## 7. PaCER

The algorithm is specified in `10` (Husch et al. 2017). This section is the interface.

**Build.** `crates/dbsloc-pacer` is licensed AGPL-3.0-only (`docs/LICENSING.md`, *PaCER and the AGPL*) and is an optional dependency of `dbsloc-pipeline` behind the cargo feature `pacer`, on by default and forwarded by `dbsloc-cli`, `dbsloc-app` and `dbsloc-server` (owner decision 2026-11-13). Without it (`ct_recon.rs`, `PACER_AVAILABLE`), `recon_method = pacer` fails with `E_RECON_pacer_unavailable: the reconstruction method pacer (--recon-method pacer) needs the optional PaCER component (licensed AGPL-3.0-only), which this build does not include; use auto or trac_core`: `dbsloc run` exits 2 before planning, the pipeline driver fails a run that includes `reconstruct` before its first stage (`crates/dbsloc-pipeline/src/lib.rs`), and the stage repeats the check (§3.2). `auto` never runs PaCER; it only honours a pre-exported result (§3.4).

**Inputs.** The coregistered CT, the template and the native → template transform (blockers of §3.3). When no per-subject import file exists, the stage first carries the template brain mask to the anchor and raw CT grids (`crates/dbsloc-pipeline/src/ct_mask.rs`, `fn gen_ct_mask`, `02` §7.1). PaCER uses the first of these masks that lies on the grid of the CT it segments (`meta.pacer_mask = "template"`); otherwise it computes its own (`"own"`, with `dbsloc: note: reconstruct: no template brain mask lies on the CT grid PaCER segments; PaCER computes its own`). A missing piece only logs a note ending in `dbsloc: note: no template brain mask; PaCER computes its own`.

**Call.** `dbsloc_pacer::pacer_leads(volume, config, Some(el_model))` returns one `PacerLead` per accepted metal component: its distal centre (mean of the refined centreline over the distal 20 mm) and either a placed `PacerSide` (`head`, `tail`, `x`, `y`, one point per level) or the reason it could not be placed. The voxel size comes from the CT affine; `distal_bloom_mm` follows the request when set.

**Sides.** The head midline is the mean world *x* of head-tissue voxels inside the CT's field of view (`10` §5a); without any the stage fails with `PaCER: no head tissue in the CT to find the midline`. A lead whose distal centre lies right of the midline is side `"1"`, left of it side `"2"`; it must lie at least 2 mm off the midline. Failures: `PaCER: no lead accepted`, `PaCER: <n> metal components accepted as leads; expected one or two`, `PaCER: a lead at x = <x> mm is within 2 mm of the head midline (x = <m> mm); its hemisphere is ambiguous`, `PaCER: both leads lie <right|left> of the head midline (x = <m> mm)`. Sides kept from a manual review are skipped. Any other accepted lead that cannot be placed fails the stage: `PaCER could not place <n> of <m> lead(s): <right|left> lead: <reason> | …`. PaCER never substitutes another algorithm. One lead found and placed gives a unilateral file; `meta.sides_found` lists the found sides by name.

**Output.** The native block holds PaCER's markers, `coords_mm` with one row per level, the §5 trajectory (50 points) and `trajvector`. The native span, pitch, shaft-radius and field-of-view gates run (§4.5), then the point warp (§6), §6.3, and the template tilt and brain-mask gates. `method = "pacer_rust"`, `meta.tracer = "pacer_rust"`, `meta.traced_space = "native"`, `meta.midline_x_mm`, `meta.pacer_mask`.

---

## 8. Reconstruction file (Reco v2)

### 8.1 Location and writing

`reconstruction/sub-<id>_desc-reconstruction.json` (`crates/dbsloc-pipeline/src/derivatives.rs`, `fn reconstruction_json`). Writers: the `reconstruct` stage, the orientation stage (`06`) and manual correction (§9a). The file is JSON with 2-space indentation and a trailing newline, written to `.partial-<name>` and renamed (`outputs::write_replacing`), so a failed write leaves the previous file intact. Numbers are written in the shortest form that reads back to the same double. Keys keep their insertion order; readers must not depend on it.

### 8.2 Top-level fields

| Field | Content |
|---|---|
| `version` | `2`; readers refuse any other value |
| `subj_id` | subject id without `sub-` |
| `method` | `trac_core_rust_native` (native-frame TRAC/CORE), `trac_core_rust` (template path), `pacer_rust` (PaCER); an import keeps its own, `pacer_import` when empty |
| `mni` | template-space sides (§2) |
| `reference` | optional, directly after `mni` (§8.5) |
| `native` | native sides; absent on the template path |
| `props` | per side in the file: `{elmodel, manually_corrected}` |
| `meta` | §8.4 |
| `scrf` | optional brain-shift-corrected native sides (Lead-DBS); read by the VAT stage (`07`) and converted by §8.8, never written by this program |
| `acpc` | optional (Lead-DBS); passed through by the Python reader, never written |

### 8.3 Side keys and hemispheres (normative)

Side `"1"` is the right hemisphere (positive RAS *x*) and side `"2"` the left, the order of `09` §1 (`contactnames` right first). The rule applies to the keys of `native`, `mni`, `reference`, `scrf` and `props` and to the `meta` maps keyed by side (`reconstruct.status`, `reconstruct.failed`, `reconstruct.flags`, `manual_correction`, `orientation`). `meta.side_source`, `meta.metal_axis` and `meta.side_notes` are keyed by `left` / `right` instead. Writers record `meta.side_keys = "1=right,2=left"`; this program wrote the opposite order before 2026-10-04.

**Readers resolve hemispheres, never raw keys.** A file that declares `meta.side_keys = "1=right,2=left"` is read by key. A file without it is read by the mean *x* of each side (the mean of `head` and `tail` *x*, else of the trajectory, else of the contacts): of two sides the larger is right, a single side goes by its sign. Any other declared value, two sides at the same mean *x*, or more than two sides is an error. The helpers are `dbsloc/reconstruction/io.py` (`side_hemispheres`, `side_key_for`), `crates/dbsloc-pipeline/src/tutor.rs` (`fn side_hemispheres`, used by Lead-Tutor and the VAT stage) and `crates/dbsloc-viz/src/reco_json.rs` (viewer). Lead-Tutor `leftElectrode` = right hemisphere = Lead-DBS side 1.

### 8.4 `meta`

| Key | Written by | Content |
|---|---|---|
| `native_input` (native CT, PaCER), `norm_inputs` (template path), `nifti_aff_4x4` (both TRAC/CORE paths) | as listed | traced CT path, traced normalized images, voxel-to-world affine of the traced volume |
| `frame_rigid_4x4`, `frame_scale`, `frame_fit_rms_mm` | native CT | frame of §4.4 |
| `lead_diameter_mm`, `eldist_mm`, `elmodel`, `n_contacts`, `level_z_mm`, `contact_level`, `is_directed` | every traced path | the electrode model (`09`) |
| `tracer`, `side_keys` | every traced path | `trac_core_rust` or `pacer_rust`; `1=right,2=left` |
| `traced_space`, `hybridsave` | native CT, PaCER | `native`; `native -> mni` |
| `side_source`, `metal_axis`, `side_notes` | native CT | §4.4 |
| `sides_found`, `midline_x_mm`, `pacer_mask` | PaCER | §7 |
| `pacer`, `pacer_import_warnings` | import | §3.4 |
| `orientation` | TRAC/CORE and PaCER writers for directional models, orientation stage, manual correction | §8.9 |
| `reconstruct` | every `reconstruct` write | §8.6 |
| `template_contacts` | §6.3 | `from_markers` |
| `template_space` | every `reconstruct` write with a template | the template's space id (`04` §3.1), kept when already present |
| `software`, `run_options`, `settings` | every `reconstruct` write | §8.7 |
| `manual_correction` | §9a | per-side review records |
| `tail_fiducial` | every writer | `fourth_level` (§8.8) |

### 8.5 Reference block

`mni` is in the space of the normalization template. When that space is not `MNI152NLin2009bAsym`, every `reconstruct` write adds a top-level `reference` object directly after `mni`: `"space": "MNI152NLin2009bAsym"` plus the sides, built from the `mni` sides through the template bridge (`04` §5.4a; `space_bridge.rs`, `fn reference_block`, `Bridge::points_to_reference`). The individually mapped contacts (`coords_mm_warped` where present, else `coords_mm`), the trajectory and the markers are mapped; `x` / `y` follow the construction of §6.2 from the template `y`; `trajvector` is recomputed; §6.3 then runs on the block. Hemispheres resolve as in §8.3. If the bridge cannot be built, the file is written without `reference` and the run logs `dbsloc: warning: reconstruct: no reference-space coordinates (<why>); Lead-Tutor needs MNI152NLin2009bAsym coordinates` (`<why>` is `the template space bridge is unavailable` under `DBSLOC_NO_BRIDGE=1`). Same-space runs have no `reference` key. Consumers bound to the reference space (Lead-Tutor) read `reference` when `meta.template_space` differs and refuse a file without it. A manual correction rebuilds the block only when the file already has one (§9a).

### 8.6 Reconstruction status (`meta.reconstruct`, normative 2026-10-16)

Every file `reconstruct` writes carries `meta.reconstruct = {status: {<k>: "traced" | "kept" | "failed"}, failed: {<k>: <reason>}, flags: {<k>: [<flag>, …]}, inputs: [<fingerprint>]}`, side keys of §8.3 with `"1"` first.

- `status` has an entry per side the run handled: `traced` (the side's position was produced in this run, by tracing, or brought in this run by an import), `kept` (kept from a manual review, §9a), `failed` (left out, §4.4 *One lead*). A side PaCER did not find has no entry (`meta.sides_found` tells which were found). The import marker `meta.pacer == "import_json"` tells an imported file from a traced one. An import (Reco v2 JSON, §5) keeps the flags of the file's own `meta.reconstruct`; for a side the import does not bring it keeps only a `failed` status with its reason (a `traced` or `kept` status the file carried for such a side is dropped); it sets the status of every side its `native` block brings and the run does not keep to `traced`, removes that side's `failed` reason (and the `failed` map when it ends up empty) and adds an empty flag array for it when the file had none; the methods text (`00` §5.3) then names the imported sides (2026-11-15; before, the file's maps were kept unchanged, and an import without them got empty `status` and `flags` maps).
- `failed` is present only when a side failed.
- `flags` has an array for every traced or kept side (possibly empty). The defined flag is `vertical_marker` (§6.2). A kept side keeps its previous entry; a native-space manual correction rewrites the `vertical_marker` entry of each edited side from its re-warp. The template path writes empty arrays.
- `inputs` is the input fingerprint of the run, the list defined in §9a *Per-side review*, taken when the file is written. When it cannot be taken it is `null` and the run logs `dbsloc: warning: reconstruct: the input fingerprint could not be recorded (<error>); its leads cannot be accepted until reconstruct runs again`. It records with which coregistration and normalization the traced sides' coordinates were made. Every `reconstruct` run writes it, an import included; a manual correction never changes it.

### 8.7 Provenance (normative 2026-10-22)

Every file `reconstruct` writes carries:

- `meta.software = {product, version, git_hash, target_os, target_arch, written_at}`: the build that wrote the file, `written_at` in RFC 3339 UTC; replaced on every `reconstruct` write, an import included.
- `meta.run_options`: the serialised `RunOptions` of the stage without `el_model_error`; `el_model` is the resolved one.
- `meta.settings = {recon_method, recon_method_requested, el_model, recon_bright_metal, template, template_space, reg_backend: {preprocess, coregister, normalize}, norm_isotropic_mm, norm_multispectral}`. `recon_method` is the effective method derived from `method` (`trac_core_native`, `trac_core`, `pacer`, `pacer_import`, else the requested value); `recon_bright_metal` is `false` on the CT path; the backends come from the method logs of those stages: `null` when a log is missing; a log without a backend key gives `"ants"` for `preprocess` and `normalize` and `null` for `coregister` (`stage_impl.rs`, `fn reconstruction_settings`).

A manual correction and the orientation stage keep `meta.software` and add `software: {version, git_hash}` to every record they write (`meta.manual_correction[<k>]`, `meta.orientation[<k>]`). The input fingerprint hashes input files, never the reconstruction file, so `written_at` does not change it.

### 8.8 Tail fiducial convention (normative, 2026-11-14)

`markers.tail` is the model tail fiducial of `09` §2.2, the convention of Lead-DBS (catalogue field `tail_level`, `09` §2.1). Every writer records `meta.tail_fiducial = "fourth_level"` (`crates/dbsloc-reconstruct/src/tail_fiducial.rs`, `fn mark_tail_fiducial`; Python `write_recon_json_v2`). Files this program wrote before 2026-11-14 put the tail on the **last** level, which differs from the tail fiducial only for models with more than 4 levels.

**Conversion on read** (`fn convert_legacy_tail`; Python `dbsloc/reconstruction/io.py`, `convert_legacy_tail`). For a file without `meta.tail_fiducial`, per side whose model (`props[<k>].elmodel`, else `meta.elmodel`, else the caller's model) has more than 4 levels: the native `|tail − head|` (the `mni` markers when the `native` block lacks that side or its `head` / `tail`) is compared with the model's last-level span `z_last − z_h` and with `fiducial_span`. When it is strictly nearer the last-level span, that side is converted in memory before any other use: in every block (`native`, `mni`, `reference`, `scrf`) `tail ← head + ((z_t − z_h) / (z_last − z_h)) · (tail − head)`, and a stored trajectory is rebuilt by §5. `coords_mm`, `x` and `y` are kept. The conversion logs `dbsloc: warning: reconstruction: side <k>: tail marker moved from the last to the 4th contact level (file written before 2026-11-14)`; the next write stores the converted markers and the field. A file whose distance is nearer `fiducial_span` (a Lead-DBS file) is used unchanged.

Every reader that combines the markers with the electrode model converts: the PaCER import, manual correction (including kept sides of a re-run), orientation, VAT, the Lead-Tutor export, the `.mat` export, the group calculation, the group view and Lead Group export, the 3D viewer and the Python reader `read_recon_json_v2`. The warnings reach the run log and the group notes; `dbsloc reco show` and the *Refine localization* view list each converted side as `<Right|Left> lead: the tail marker was moved from the last to the 4th contact level, because the file was written before 2026-11-14. Save to keep the corrected marker.`

### 8.9 Orientation note

For a directional lead, `x` / `y` are an arbitrary in-plane basis until the orientation stage (`06`) solves the segment azimuth. The TRAC/CORE writers and the PaCER writer (`ct_recon.rs`, `fn build_pacer_reco_json`, since 2026-11-15) then set `meta.orientation` to the string `unresolved: x/y markers are an arbitrary in-plane basis; segment azimuth needs the orientation stage (specs/06)`, so no consumer reads `x` as the segment direction. The orientation stage and manual correction replace it with per-side objects (`06` §10.6; §9a of this spec). A PaCER import (§3.4) is taken as it is and gets no note.

### 8.10 MATLAB twin (normative 2026-10-22)

Every write of the reconstruction JSON (reconstruct, orientation, manual correction) also writes `sub-<id>_desc-reconstruction.mat` beside it in the Lead-DBS format of `12` Part 1 (`crates/dbsloc-pipeline/src/reco_mat.rs`, `fn export_beside_json`). A failure logs `dbsloc: warning: reconstruct: the Lead-DBS compatible <file> was not written: <error>` and never fails the writer. The `.mat` is derived from the JSON and never read back by the pipeline.

---

## 9. Stage outputs

A successful run (`stage_impl.rs`, `fn finish_reconstruction`, `fn write_reconstruction`) merges the kept sides (§9a *Reconstruct re-run*) and records `meta.reconstruct.inputs` (§8.6); adds `meta.template_space` and the `reference` block (§8.5) and restores the kept sides' `reference` entries; stamps the provenance (§8.7) and `meta.tail_fiducial` (§8.8); writes the JSON (§8.1) and the `.mat` twin (§8.10); when the template file exists, writes the 3D scene manifest `reconstruction/viz/scene_manifest.json` (`08`) and opens the scene unless the run is headless or `no_scene3d` is set (a manifest failure only logs `[reconstruct] scene manifest write failed: <error>`); settles the `reconstruct` lock from the reviews (§9a); and reports `ok:reconstruct:sub-<id>_desc-reconstruction.json`. A PaCER run also leaves the CT brain masks of §7 in `reconstruction/` (`derivatives.rs`, `fn brain_mask_anchor`, `fn brain_mask_ct`).

---

## 9a. Manual correction (normative, 2026-10-14)

An expert may move a lead and set its roll after any reconstruction. The model is the one of the Lead-DBS correction tool: the user edits only the **head** (distal) and **tail** (proximal) markers and the **roll** of one side; contacts, trajectory and ring-plane markers are always rebuilt from the electrode model (§5), never edited one by one. Code: `crates/dbsloc-pipeline/src/manual_correction.rs`.

**Entry points.**

- Library: `apply_manual_correction(sub, opts, request, cancel, log)`, used by the GUI *Refine localization* view (`08` §11.1.6) and the CLI. It resolves the template and the electrode model like the stage (§3.1). `inspect_manual_correction` returns the edit context without needing the backdrop image; `prepare_manual_correction` requires it. `file_edit_space(sub)` returns the file's edit space.
- CLI (`crates/dbsloc-cli/src/cmd/reco.rs`): `dbsloc reco correct <dataset_root> <subject> [--side right|left --head X,Y,Z --tail X,Y,Z [--roll DEG]] [--accept right|left]… [--space native|mni] [--source gui|cli] [--el-model M] [--template P] [--reg-backend auto|wgpu|ants] [--normalize-backend auto|wgpu|ants] [--no-index] [--output-dir DIR]`. `--side`, `--head` and `--tail` go together (one correction per call); `--accept` is repeatable; at least one of the two is required, else `E_RECON_manual: nothing to do: give --side with --head and --tail, or --accept right|left`. `--space` defaults to the file's edit space.
- CLI: `dbsloc reco show <dataset_root> <subject> [--el-model M] [--no-index] [--output-dir DIR]` prints the edit context as JSON: `subj_id`, `space`, `backdrop_nii` (empty when missing), per side `key`, `label`, `elmodel`, `head`, `tail`, `x`, `y`, `roll_deg`, `candidates_deg`, `orientation_status`, `manually_corrected`, `review`, `roll_review`, plus `warnings` (the tail conversions of §8.8). The *Refine localization* view shows the same warnings in its status bar.

A request holds an edit space and one entry per side: a correction (`SideEdit {key, head, tail, roll_deg, accept: false}`) or an **accept** (`accept: true`: the side is marked as checked without an edit; `head`, `tail` and `roll_deg` are not read and no geometry, warp or block changes). A side given twice is `E_RECON_manual: side <k> given twice`. Several sides may be corrected or accepted in one library call. A side not in the request is left as it was, except that its template-space `coords_mm` and `trajectory` are recomputed from its markers (§6.3), which changes nothing in a file written under the current rules.

**Edit space.** The file decides: a file with a `native` block (CT, §3) is edited in **native** millimetres; a file with only `mni` (template path) in **template** millimetres (`meta.template_space`). A request in the other space fails with `E_RECON_manual: the reconstruction file is edited in <space> space, not <space>`. The backdrop is the coregistered post-op CT for a native edit and the normalized post-op MR in use (`01` §6.3, §4.1 of this spec) for a template edit, so the image and the edited coordinates share one frame.

**Per side** (key `"1"` right, `"2"` left, §8.3):

1. **Electrode model** = `props[<k>].elmodel`, else `meta.elmodel`, else the run's model (§3.1). Unresolved → `E_RECON_manual: no electrode model for side <k>`; unknown → `E_RECON_manual: electrode model "<name>" of side <k> is not in the electrode catalogue`. Non-finite input → `E_RECON_manual: side <k>: head and tail must be finite` or `E_RECON_manual: side <k>: roll must be finite`. `|tail − head|` below 1e-6 mm → `E_RECON_manual: head and tail coincide`.
2. **Roll** (horizontal azimuth, decision 2026-10-22). `t = unit(tail − head)`, `r = lead_diameter / 2`. The roll θ is the horizontal azimuth of `12` §11.5 / `06` §10.6: `x̃ = (cos θ, sin θ, 0)`, `x_dir = unit(x̃ − (x̃ · t) t)`, `y_dir = t × x_dir`. θ = 0 puts segment A anterior; a positive θ turns `y` toward −x. With `--roll DEG`, θ = DEG; otherwise θ is read back from the edited block's previous markers, `θ = atan2(−d_x, d_y)` with `d = markers.y − markers.head` (`orientation.rs`, `fn roll_azimuth_deg`; θ = 0 when the block has no `y` or its horizontal part is shorter than 1e-6), so a head / tail move keeps the azimuth. When `x̃` is parallel to `t` (perpendicular part shorter than 1e-6), `y_dir` is the anterior direction projected perpendicular to `t` (the +x direction when that degenerates too) rotated about `t` by θ, and `x_dir = y_dir × t`. Leads are written head = deep, tail = superior, so the read-back is single-valued.
3. **Contacts** = §5 `resolve_fiducials([head, tail, head + r · x_dir, head + r · y_dir], model)`: rigid fit, `coords_mm`, 50-point `trajectory`, `trajvector`, markers at radius `r`. The edited block (`native` or `mni`) receives this geometry.
4. **Other spaces.** Native edit: when the file has an `mni` block, the side's `mni` block is the point warp of the new native block (§6.1, same route rule as the stage), except `x` / `y`, which follow the `06` §10.6 construction from the native `y_dir` and the warped `head` / `tail` (flag `vertical_marker` as there); §6.3 runs with `coords_mm_warped` captured. Then the `reference` block (§8.5) is rebuilt for the whole file when the file has one, with the same `x` / `y` rule for the edited sides. Template edit: `mni` is the edited block (§6.3 without `coords_mm_warped`); `reference` is rebuilt as in §8.5; no native block is created. In both cases the untouched sides' previous `reference` entries are restored. When the template file is missing, the `reference` block is dropped with `dbsloc: warning: reconstruct: no reference-space coordinates (no template to rebuild the reference block)` and a `reference-space coordinates dropped: …` warning.
5. **Record.** `props[<k>].manually_corrected = true`. `meta.manual_correction[<k>] = {review: "corrected", roll_review, at: <unix seconds, float>, source: "gui" | "cli", space: "native" | "mni", previous: {head, tail, x, y}, head_shift_mm, tail_shift_mm, roll_deg: <value or null>, inputs, software}`. `previous` holds the markers of the edited block before this call; a second correction of a side overwrites that side's entry. `roll_review` and `inputs` are defined under *Per-side review*. When a roll was given, `meta.orientation[<k>]` becomes `{status: "manual", method: "manual", roll_deg: DEG, candidates_deg: null, polar_angle_deg: null, marker_slice_mm: null, harmonic_amplitude: null, com_offset_mm: null, flags: [], software}`; if `meta.orientation` was the unresolved string it becomes a per-side object holding only the edited sides. When the roll was kept, the previous orientation entry is left as it is (step 2 keeps the azimuth; a pre-2026-10-22 axis-convention `roll_deg` is superseded by the markers, which `inspect` / `prepare` read back).
6. **Plausibility is advisory.** The §4.5 tilt check and the span, pitch and shaft-radius checks (no field of view) run on the new geometry and go to the returned `warnings` as `<Right|Left> lead: <problem>`, never to an error. The expert's coordinates are authoritative.

**Per-side review (normative, 2026-10-16).** Each side carries its own review mark in `meta.manual_correction[<k>]`:

- `review`: `"corrected"` (a correction, step 5) or `"accepted"` (an accept). An accept of a side without a record writes `{review: "accepted", roll_review, at, source, space, inputs, software}`. An accept of a side that already has a record keeps it (a corrected side stays `corrected`, its `inputs` unchanged) and fills a null `roll_review` as below. `props[<k>].manually_corrected` stays `false` for an accepted side.
- **An accept needs current coordinates.** The side's coordinates were made with the record's `inputs` when the side has a record, else with `meta.reconstruct.inputs` (§8.6). An accept fails with `E_RECON_manual: side <k>: the inputs changed since the reconstruction (coregistration or normalization re-ran); run reconstruct before accepting the lead` when that fingerprint no longer matches a fresh one, and with `E_RECON_manual: side <k>: its coordinates carry no input fingerprint (the reconstruction predates it); run reconstruct before accepting the lead` when there is none. A correction rebuilds its side from the current inputs and records them for that side only; it never refreshes another side's record or `meta.reconstruct.inputs`.
- `roll_review`: `"set"` when a correction gave `--roll`; `"accepted"` when an accept confirmed a roll that was already determined, i.e. the side's model is directional (an unknown model counts as directional) and `meta.orientation[<k>].status` is `resolved`, `ambiguous` or `manual`; otherwise `null` (the roll is left to the orientation stage). A correction without `--roll` keeps the side's previous `roll_review`.
- `inputs`: the input fingerprint at the time of the review, a list of `{path, exists, mtime_ns, size, sha256}` entries in the format of `00` §6, over the `reconstruct` lock inputs of `00` §6 plus the coregistered post-op CT (when the subject has one) and the native ← template inverse composite of the normalization (when present), deduplicated (`fn correction_input_paths`). When it cannot be taken (an input exists but is unreadable), `inputs` is `null`, the warning `dbsloc: warning: manual correction: the input fingerprint could not be recorded (<error>); a later reconstruct run drops this review` is returned, and a later `reconstruct` run drops the review.
- Readers: a record without `review` (written before 2026-10-16) counts as `corrected`, with `roll_review = "set"` when its `roll_deg` is a number; `props[<k>].manually_corrected = true` without a record counts as `corrected`. `fn side_review` and `fn roll_review` apply this rule; `dbsloc reco show` and `prepare_manual_correction` report both per side.

**File write.** The JSON is rewritten through `.partial-*` and rename (§8.1) with `meta.tail_fiducial` set, followed by the `.mat` twin (§8.10). On any error before the rename the previous file is intact.

**Locks and dependents (`00` §6.1).** The stage locks are per subject and stage, the review is per side; "present" below means the sides of the edited block.

- A side **counts as reviewed** only while its record has a `review` and its `inputs` still match a fresh fingerprint; a review made before a re-run coregistration or normalization does not count.
- `locks/reconstruct.json` is written approved (`approval: true`, `method: "manual_correction"`, the stage's default input snapshot, `manually_corrected: <whether any reviewed side is corrected>`) only when **every present side** counts as reviewed, so a later run skips `reconstruct` and keeps the expert's leads; a re-run of an upstream stage (coreg, normalize, brainshift) still invalidates it as any approval. Until then an approving `reconstruct` lock is invalidated (`invalidated_by: "manual_correction"`) and `reconstruct` stays unapproved; a later run re-runs it as described under *Reconstruct re-run*.
- After a correction (a request with a non-accept entry), `vat`, `visualize` and `orientation` are invalidated as after a completed `reconstruct` (`invalidated_by: "manual_correction"`). An accept-only request changes no geometry and invalidates no dependent.
- `orientation`: its lock is written approved (`method: "manual_correction"`) when at least one present side is directional and **every present directional side** counts as reviewed and has a `roll_review` (`set` or `accepted`), so the automatic estimate does not overwrite the expert's rolls; otherwise it stays invalidated (a correction) or as it was (an accept). Ring-only files never approve it (the orientation stage skips them anyway, `06` §10.6).
- The log lines `dbsloc: manual correction: approvals invalidated: <stages>` and `dbsloc: manual correction: approved: <stages>` list what changed. Lock failures after the file was written are warnings (`lock: …`), never errors, because the correction itself is on disk.

**Reconstruct re-run (normative, 2026-10-16).** When `reconstruct` runs while the existing file has reviewed sides, each reviewed side is **kept** from the existing file, untraced, when all of the following hold: the file's edit block (`native` on the CT path, `mni` on the template path) has the side; the record's `space` matches that block; the record has `inputs` and they still match a fresh fingerprint of the same paths (`00` §6 comparison); the side's `props[<k>].elmodel` names the same catalogue entry as the run's electrode model when both are known (names compare as the catalogue lookup does). Otherwise the review is dropped with `dbsloc: warning: reconstruct: the manual review of the <left|right> lead (side <k>) is dropped: <why>; the lead is traced again`, `<why>` being `the <block> block has no side <k>`, `it was made in another space than the <space> space this run reconstructs in`, `no input fingerprint was recorded with it`, `the coregistration or normalization changed since it was made, so its coordinates no longer fit` or `it was made for the electrode model <a>, not <b>`. A kept side is logged with `dbsloc: note: reconstruct: the <left|right> lead (side <k>) is kept as reviewed in a manual correction (<review>)`.

The sides not kept are traced (§4, §7 or the import of §3.4). The stage then copies, for each kept side, its `native` and `mni` entries, `props[<k>]`, `meta.manual_correction[<k>]` (records of dropped sides are removed), `meta.orientation[<k>]` when it is an object (an unresolved string becomes a per-side object, as in step 5), and its `side_source`, `metal_axis` and `side_notes` entries, and sets `meta.reconstruct.status[<k>] = "kept"` with its previous `flags`. The `reference` block is rebuilt from `mni` (§8.5) and the kept sides' previous `reference` entries are restored. On the template path a side that fails to trace next to a kept side is left out and recorded as on the CT path (§4.4).

After the write, when every side in the file counts as reviewed, the stage writes the `reconstruct` lock approved as under *Locks and dependents* and logs `dbsloc: note: reconstruct: every lead in the file was reviewed in a manual correction; the reconstruct step stays approved`. Otherwise an approving `reconstruct` lock whose `method` is `manual_correction` is invalidated (`invalidated_by: "reconstruct"`, log `dbsloc: note: reconstruct: not every lead in the file is reviewed any more; the approval from the manual correction is withdrawn`), so a run that dropped reviews does not leave the stage approved; an approval written by other means (`dbsloc lock approve`) is left as it is. The completed stage invalidates its dependents as usual (`00` §6.1).

**Rerun all selected steps.** `RunOptions.overwrite_approved` (`--overwrite-approved`, GUI *Rerun all selected steps*) re-traces **every** lead and drops every review with `dbsloc: warning: reconstruct: Rerun all selected steps re-traces every lead; the manual review of <sides> is dropped` (`<sides>` such as `the right lead (side 1) and the left lead (side 2)`); the GUI warns on the Reconstruct card when any lead is corrected or accepted. On the orientation stage it estimates the roll of every directional side again and clears its `roll_review` (`06` §10.6).

**Return value.** `CorrectionReport {path, sides: [{key, space, review, head_shift_mm, tail_shift_mm, roll_deg}], warnings, invalidated, approved}`: corrections first, then accepts; an accept reports shifts of 0 and `roll_deg: null`; `roll_deg` is the roll given in the request, `null` when kept; `invalidated` lists the stages whose approval was actually cleared (`reconstruct` included when an approval was revoked, `orientation` absent when it was re-approved); `approved` lists the locks written approved. `dbsloc reco correct` prints it as JSON and exits 0. Exit codes: 1 for `E_RECON_manual`, `E_RECON_warp` and other pipeline errors, 2 for subject-resolution (BIDS) errors and command-line usage errors, 130 on Ctrl-C.

**Errors.** `E_RECON_manual: <reason>`, with these reasons besides those above: `no sides to correct`; `unknown source "<s>" (gui or cli)`; `unknown side "<k>" (1 or 2)` (CLI: `unknown side "<s>" (right or left)`); `no reconstruction file at <path>`; `cannot read <path>: <error>`; `cannot parse <path>: <error>`; `<path> is not a Reco v2 file (version 2 required)`; `the reconstruction file has no lead sides`; `side <k> is absent from the <block> block`; `the mni markers of side <k> lack head / tail` (step 4); `side <k> of the <block> block has no head / tail markers` (`inspect` / `prepare`); `backdrop image missing: <path>` and `no normalized post-op MR image to show the template-space edit` (`prepare` only); CLI parsing: `--<head|tail> expects X,Y,Z in millimetres, got "<value>"`, `unknown space "<s>" (native or mni)`, `a correction needs --side, --head and --tail together (--roll is optional)`. `E_RECON_warp: <reason>` comes from step 4; the file and the locks are then untouched.

**Acceptance** (`manual_correction::tests`). (a) Correcting a synthetic reconstruction with its automatic `head` / `tail` and no roll leaves `coords_mm`, `x` and `y` within 1e-6 mm. (b) Shifting `head` and `tail` by `(1, 2, 3)` mm shifts every contact by that vector within 1e-6 mm and keeps `roll_azimuth_deg(y − head)` within 1e-6°. (c) `--roll 30` gives `roll_azimuth_deg(y − head) = 30 ± 1e-6` and `|x − head| = |y − head| = r`. (d) With an identity native → template map the `mni` block equals the native block after a native edit. (e) The untouched side's JSON is byte-identical. (f) Correcting both sides with a roll approves `reconstruct` and `orientation` and invalidates `vat` / `visualize`; correcting one of two sides leaves both unapproved; a correction without a roll plus an accept of the other side approves `reconstruct` and invalidates `orientation`. (g) A template-only file rejects `--space native`. (h) An accept changes no block, invalidates no dependent and records `review: "accepted"` (`roll_review: "accepted"` for a resolved orientation, `null` for an unresolved one). (i) `reconstruct` with every side reviewed and unchanged inputs keeps both sides byte-identical, records `status: "kept"` and approves its lock; a changed fingerprint drops that side's review; `overwrite_approved` drops all and withdraws a `manual_correction` approval. (j) `orientation` with every directional roll reviewed is skipped; with `overwrite_approved` it estimates again. (k) After a correction of side 1 and a changed input, an accept of either side is refused, and a correction of side 2 leaves side 1's record untouched and does not approve `reconstruct`. (l) Electrode models compare as catalogue entries (case, spacing and the catalogue file name are aliases).

---

## 10. Errors and warnings

The exact texts are quoted in the sections; the `recon:` prefix rule is in §3.2. Warnings and notes never fail the stage.

| Code | Section |
|---|---|
| prerequisite messages, `no electrode model set; …`, `E_RECON_elmodel` | §3.1 |
| `E_RECON` (unknown method, method needs a CT), `E_PACER_mri` | §3.2 |
| `E_RECON_pacer_unavailable` | §7 |
| `E_RECON_native`, `E_RECON_input` | §3.3, §4.1 |
| `RecoFileV2 …` | §3.4 |
| TRAC/CORE side errors, `E_RECON_trace` | §4.2, §4.4 |
| TRAC-window bridge error | §4.3 |
| `E_RECON_implausible` | §4.5 |
| `head and tail coincide` | §5 |
| `E_RECON_warp` | §6.1 |
| `PaCER: …` | §7 |
| `E_RECON_manual` | §9a |

---

## 11. Acceptance criteria

- `crates/dbsloc-reconstruct/tests/trac_core_phantom.rs`: on synthetic leads of six models (metal dark and bright) `head` lies within 1.2 mm of the truth and 0.3 mm along the shaft, `|tail − head|` within 0.2 mm of `fiducial_span`; skull-only seeds are refused; the reference-space windows behave as in §4.3.
- `crates/dbsloc-reconstruct/src/native_frame.rs` tests: frame fit, axis refinement, continuity-only corroboration, `contacts_from_markers` (§6.3).
- `crates/dbsloc-reconstruct/src/plausibility.rs` tests: every gate of §4.5, including the Euclidean brain-mask margin (`brain_mask_margin_is_a_euclidean_distance`: 4.3 mm from the mask fails a 3 mm margin although within 3 mm on every axis).
- `crates/dbsloc-pipeline/src/ct_recon.rs` tests: both spaces through the point warp, metal-axis fallback still facing the tilt gate, one lead, kept sides, PaCER import for `auto` / `pacer` only, method parsing, the build without `pacer`, the warp routes of §6.1.
- `crates/dbsloc-reconstruct/src/tail_fiducial.rs` tests and `tests/test_reconstruction_io_tail.py`: the conversion of §8.8.
- `crates/dbsloc-pipeline/src/manual_correction.rs` tests: §9a *Acceptance*.
- `tests/test_ct_reconstruct.py` (research binding) and `tests/test_e2e_synthetic.py` (`dbsloc` binary): end to end.

---

## 12. References

- TRAC/CORE and manual correction (cited by the methods text, `00` §5.3): Horn A, Kühn AA (2015) Lead-DBS: A toolbox for deep brain stimulation electrode localizations and visualizations. *NeuroImage* 107:127–135. doi:10.1016/j.neuroimage.2014.12.002.
- PaCER (cited by the methods text, `00` §5.3): Husch A, Petersen MV, Gemmar P, Goncalves J, Hertel F (2017) PaCER — A fully automated method for electrode trajectory and contact reconstruction in deep brain stimulation. *NeuroImage Clin* 17:80–89. doi:10.1016/j.nicl.2017.10.004.
- Horn A et al. (2019). Lead-DBS v2: Towards a comprehensive pipeline for deep brain stimulation imaging. *NeuroImage* 184, 293–316.
