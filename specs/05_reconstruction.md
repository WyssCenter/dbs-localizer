# Spec 05 — Electrode Reconstruction (Wyss DBS Localizer)

**Status:** Phase B  
**Depends on:** `01_bids`, `04_normalization` (for TRAC/CORE input space), `09_electrode_models`  
**Phase A (detail, archived):** the Phase A archive (removed 2026-10-23, in git history) — section `specs/phase_a_05_reconstruction_report.md`

---

## 1. Purpose

Localize DBS lead contacts in 3D from post-operative **CT** or **MRI**. Three method classes:


| ID          | Name           | Post-op     | Working space (reference) |
| ----------- | -------------- | ----------- | ------------------------- |
| `trac_core` | TRAC/CORE      | CT or MR    | **Template** (normalized) |
| `pacer`     | PaCER          | **CT only** | **Native** (coreg CT)     |
| `manual`    | Fiducial click | CT or MR    | **Native** (coreg)        |


**Out of scope:** full PaCER source port — **subprocess/contract** or stub for Phase C.

---

## 2. Common outputs (all methods)

Per **side** (L/R) the pipeline produces:

- `**coords_mm`:** `N_c × 3` float, mm, **RAS** world space for that result space.  
- `**trajectory`:** `50 × 3` mm — equidistant line along the shaft (see §7).  
- `**markers`:** struct with at least `head`, `tail`, `x`, `y` (each `1×3` mm) in same space.  
- `**trajvector`:** `1×3` unit, tip→skull (distal to proximal) — stored where applicable.

**Contact index 0** = most **distal** (deepest) contact; last index = most **proximal** toward skull. Aligns with `elspec` contact order in `09`.

---

## 3. Dispatch

**Input:** `recon_method` enum, `Subject`, `Elspec`, `sides` ∈ {1,2}.

**TRAC/CORE** requires **normalized** post-op volumes in `norm.anat.postop` and template tools (`bb` mask, etc.).

**PaCER** requires `gen_ct_mask` outputs (if used), coreg CT, brain masks per `01`.

**Manual** requires interactive UI or CLI with four fiducial picks per side in implementation.

**Post-run:** `save_reconstruction` (§6) with `hybridsave` to fill native **and** MNI (and optional AC-PC) per policy.

**Wyss DBS Localizer dispatch (normative since 2026-10-04, roadmap 4.8; one stage for every front end since 4.9).** The `reconstruct` stage (`crates/dbsloc-pipeline/src/stage_impl.rs` `run_reconstruct`) serves the GUI, `dbsloc_pipeline_runner` and `dbsloc run`; the CT path is `ct_recon.rs` (also exposed as `dbsloc_reconstruct.reconstruct_ct`), the MRI path `reconstruct_template` (Reco v2 with `method`/`meta.tracer` `trac_core_rust`, `props`, `meta.nifti_aff_4x4`, `meta.orientation` for directed leads; template-space plausibility and tilt gates of §4.5). "Post-op" is `Subject.postop_modality` (`specs/01` §10: CT + MR counts as MRI). The electrode model is resolved inside `run_reconstruct`, so every front end behaves alike: an explicit, non-blank `RunOptions.el_model` (`--el-model`, GUI picker) wins; otherwise the `elmodel` variable of the subject's `prefs/sub-<id>_desc-uiprefs.mat` (MATLAB v7.3, read in pure Rust by `crates/dbsloc-pipeline/src/uiprefs.rs`) is used and the run log carries `dbsloc: note: reconstruct: electrode model from <uiprefs path>: <model>`; when neither exists the stage fails with the `el_model_error` text (`no electrode model set; ...`). An unreadable or empty `uiprefs.mat` counts as absent. There is no default lead:

| Post-op | `recon_method` | Path |
| --- | --- | --- |
| CT | `auto`, `pacer` | a pre-exported PaCER Reco v2 JSON (`$DBSLOC_PACER_RECO_JSON`, else `<recon_dir>/sub-<id>_desc-pacer_import.json`) is honoured first (`meta.pacer = "import_json"`, `meta.pacer_import_warnings` when incomplete) |
| CT | `auto`, `trac_core`, `trac_core_native` | native-frame TRAC/CORE (§4.4–4.5) |
| CT | `pacer` | Rust PaCER (§7) in native space, `mni` block from the §6 point warp |
| MRI | `auto`, `trac_core` | template-space TRAC/CORE (§4.1–4.3) |
| MRI | `pacer` / `trac_core_native` | `E_PACER_mri` / `E_RECON` |

**PaCER import validation.** The import file must be Reco `version` 2, its `subj_id` (with or without `sub-`) must equal the requested subject, its `mni` block must be a non-empty object and its `native` block must hold at least one side object; otherwise the stage fails with `Invalid` (`RecoFileV2 <path>: <why>`, e.g. `the import is for subject "<id>", not "<id>"`, `missing or empty mni block`, `no native block with a lead side; …`). A file named by `$DBSLOC_PACER_RECO_JSON` without `subj_id` is `Invalid` (it cannot be matched to the subject); the per-subject file in `<recon_dir>` may omit `subj_id` (its location names the subject). Per-side problems (a side that is not an object, or lacks `markers.head` / `markers.tail`) are not errors: they are listed in `meta.pacer_import_warnings`.

On CT nothing falls back to the template-space path: a missing template, coregistered CT or native→template transform is `E_RECON_native: <what is missing>`, checked before the electrode model is resolved.

---

## 4. TRAC/CORE (normative algorithm shape)

**Space:** all geometry in **template/MNI** voxel + mm for core loop.

**4.1 Composite volume**

- Build `lpost`: combine normalized post-op channels (ax/cor/sag/CT) with `bb` mask; for **CT** invert intensities (bright metal → dark) so the same “dark artifact” rule applies.  
- Sum channels per reference (class: weighted sum of masked images, float32).

**4.2 Per-side `reconstruct`**

- **Refinement loop** (0‥`refine_steps`): `reconstruct_trajectory` updates centerline.  
- **Trajectory trace** (axial-dominant):  
  - `start_z_mm` / `end_z_mm` from `spacedef` guide (defaults e.g. +8.7 / −15.5 for STN-class targets).  
  - Entry **auto** mode: half-volume crop, R/L split, threshold projections, `findpeaks` + S-G smoothing → seed mask; or **manual** `ginput` or **heuristic** `masksz` from template.  
  - **Slice loop:** for each step toward deeper brain, threshold slice, centroid with damped update toward predicted point, plausibility radius vs `end_tolerance`, `fit_line` on accumulated centerline, expand square **mask** window of half-width `mask_window` for next slice.
- **Coords along shaft:** from trajectory fit + inter-contact **pitch** = `elspec.eldist` in mm; place `N_c` points along axis.  
- **Markers:** `head` = centre of the most distal level, `tail` = the model tail fiducial of `09` §2.2 (centre of the 4th level, or of the last level when the model has fewer than 4), placed `fiducial_span` along the traced axis from `head`; `x`,`y` = offsets perpendicular using lead diameter/2.  
- **Apply `resolve_coords`:** rigid transform from `electrode` template `mat` (see `09` + §5).
- **CORE placement (Wyss DBS Localizer):** the metal's half-maximum **distal edge** along the fitted line is located on the transverse disc-maximum profile; contact 0's level centre is placed at that edge **plus the model's apex-to-level offset plus a distal bloom allowance of 0.5 mm** up the shaft (`TracConfig::distal_bloom_mm`; phantoms painted without blooming use 0). Evidence (roadmap 2.25, 2026-10-05): the previous 1.0 mm (roadmap 2.21) was calibrated through the old affine CT coreg and a SyN registered on a template that the isotropic resample had shrunk ~0.7 % toward its corner. Re-measured with both fixed: on `sub-76325` (Medtronic 3387, contacts resolved as separate peaks in the raw CT) the true level centres sit 0.6–1.0 mm deeper than the 1.0 mm markers, i.e. a bloom of 0.0–0.6 mm; against the Lead-Tutor experts the 10-subject mean error per lead is 1.651 / 1.526 / 1.499 / 1.489 mm at bloom 1.0 / 0.65 / 0.5 / 0.35 mm (leave-one-subject-out gain ≈ 0.18 mm). 0.5 mm is chosen as robust across both measurements.

**4.3 Parameters (config / `Prefs`)**

- `refine_steps`, `mask_window`, `tra_std_factor`, `end_tolerance`, `entry` target preset.
- **Seed slab (2026-10-13).** The auto entry seed thresholds the mean of the start slice and the slices after it over a slab of `seed_slab_mm` = 5.0 mm, in the volume's own millimetres along the slice axis: `n = max(1, round(seed_slab_mm / slice_spacing))` slices. It was a fixed 5 slices, whose depth followed the grid: 5 mm on a 1 mm grid, 7.5 mm on the 1.5 mm anchor of `sub-76325`, 15 mm on the 3 mm synthetic template of `tests/test_e2e_synthetic.py`, where the oblique post-op MR artifact smeared out of the threshold and the seed failed on one engine and not the other. Unchanged on 1 mm grids; on the Lead-Tutor cohort (reconstruct re-run on `hpdbs_runs/final_2026-10-13`, `sub-76325` now 3 slices, `sub-33544` 4) every fiducial is bit-identical.
- **Reference-space windows.** The start / end planes (`start_z_mm` 8.7, `end_z_mm` -15.5), the seed ellipse (semi-axes 45 x 60 mm) and the native-frame brain box of §4.4 are millimetres of the reference space `MNI152NLin2009bAsym` (`specs/04` §5.4a). `TracConfig::world_to_reference` (`Option<Mat4>`, default `None` = the volume is already in that space, behaviour unchanged) is the row-major 4x4 from the volume's world millimetres to reference millimetres. When it is set, the seed-region test and the hemisphere split evaluate `M * world`; a start / end plane is the slice at which the reference-space line `(0, 0, z)` crosses the grid (`M^-1 * (0, 0, z)` mapped to voxels); every other quantity (window half-width, plausibility radius, areas, profile radius) stays in the volume's own millimetres. A singular `M` is `TracError::Affine`. The pipeline sets it from `Bridge::affine_to_reference` (`specs/04` §5.4a) whenever the run's template space is not the reference space, for the post-op MRI template-space path and, as `M * frame * world`, for the native-frame CT path (the frame of §4.4 is fitted native -> template, so its coordinates are template-space millimetres up to the frame scale, which `seed_scaled_trac_config` keeps). If the bridge cannot be built the stage fails with `reconstruct: template space <s> needs the <ref> bridge for the TRAC search windows: <error>`; with `DBSLOC_NO_BRIDGE=1` the reference windows are used unchanged.

**4.4 Native-frame TRAC/CORE and the metal-axis path (Wyss DBS Localizer, post-op CT)**

On CT the default path traces the **coregistered native CT** under a rigid template-like frame (roadmap 4.6) and warps only the fiducials to the template.

**Frame.** A 3 × 3 × 3 lattice spanning the central 50 % of the CT's world bounding box (corner voxel centres, NIfTI best affine) is sent through the §6 point warp; the least-squares rigid fit (Kabsch) of lattice → warped lattice is the frame, `scale = sqrt(Σ|warped − mean|² / Σ|lattice − mean|²)` and `frame_fit_rms_mm` the residual of the similarity. TRAC/CORE runs on the unresampled CT with voxel-to-world `frame · affine`; its template-space seed geometry (start/end plane, seed ellipse semi-axes) is divided by `scale`. The fiducials come back through the inverse rigid map, are resolved (§5) in native space, and the native block is warped to the template. The CT is **always** negated before tracing (metal dark), and the metal-axis voxels are taken from the CT as loaded. `RunOptions.recon_bright_metal` (`--recon-bright-metal`) is an option of the template-space post-op MRI path (§4.1) only; on the native CT path it is ignored and the run logs `dbsloc: note: reconstruct: the bright metal setting does not apply to a post-op CT in HU; the CT is traced with its metal bright as scanned`.

**Electrode models.** TRAC/CORE (both paths) refuses a model with a single contact level (`<model> has a single contact level, so TRAC/CORE cannot place a head and a distinct tail on it`); PaCER is the only path for such a model.

**One lead (normative, 2026-10-16).** When exactly one side cannot be traced (neither TRAC/CORE nor a corroborated metal axis, step 3 below), the stage writes a **unilateral** reconstruction: the traced side is written as usual and the failed side is **absent** from the file (no `native`, `mni`, `reference` or `props` entry, no `meta.side_source` / `metal_axis` / `side_notes` entry). The run logs `dbsloc: warning: reconstruct: the <left|right> lead (side <k>) was not reconstructed and is left out of the file: <reason>`, and `meta.reconstruct` (§8) records `status[<k>] = "failed"` and `failed[<k>] = <reason>` (the same text as the warning, e.g. `left: TRAC/CORE: …; metal axis: …`). When **both** sides fail the stage fails with both reasons joined by ` | ` as before. The plausibility gates of §4.5 are unchanged and apply to every written side: a traced side that fails a gate fails the stage (it is not dropped as a missing side), because a gate failure points at the coregistration or the trace, not at an absent lead. Consumers read the file through the hemisphere rule of §8, which already accepts one side; Lead-Tutor (`tutor.rs`) refuses a unilateral file with its *missing MNI side(s)* error. Sides kept from a manual review (§9a *Reconstruct re-run*) are not traced; a failing traced side next to a kept side is left out in the same way, and the stage fails only when no side is traced or kept.

Per side:

1. **TRAC/CORE** as in §4.2, then, for a CT in HU, **axis refinement**: the step-2 intensity-weighted PCA refinement runs on all ≥ 1800 HU voxels of the CT (no hemisphere box; the cylinders select the lead), starting 5 mm distal of the TRAC `head` along the TRAC head → tail direction, and CORE re-runs along the refined axis with the step-2 window (4 mm below the distal end, apex-to-proximal-metal length plus 10 mm above). The refined fiducials replace the TRAC/CORE ones; if the refinement or that CORE fails, the TRAC/CORE fiducials stand. `meta.side_source` stays `trac_core`. Evidence (2026-10-12, Lead-Tutor cohort): TRAC's line direction is a robust mean of slice-to-slice centroid differences of a thresholded, linearly resampled CT, so a 0.35° change of the CT → anchor coregistration moved one traced head 1.16 mm. Re-tracing the CT under ±0.25 mm / 0.3° coregistration perturbations, the per-subject mean error had an SD of 0.21 mm (`sub-39468`) and 0.22 mm (`sub-15454`) without refinement and 0.08 / 0.15 mm with it, while the 10-subject mean stayed within 0.01 mm.
2. **Metal axis** (CT in HU only; evaluated only when TRAC/CORE cannot trace the side): voxels ≥ **1800 HU** inside the hemisphere's brain box (frame |x| < 45, −60 < y < 45, −25 < z < 70 mm, divided by the frame scale) are fitted with a RANSAC line (1.5 mm inlier radius, ≥ 30 voxels, deterministic seed), then refined by intensity-weighted PCA inside cylinders of radius 3 → 2 → 1.5 mm spanning −10 … +30 mm from the distal end. CORE (§5 placement, same metal-extent check and distal bloom) then runs along the refined axis from 4 mm below its distal end to the lead's own apex-to-proximal-metal length plus 10 mm above it (`specs/09` §3.1: most-distal level centre at `tip_length/2` from the apex when the tip is a contact, `tip_length + contact_length/2` otherwise, plus the last level depth and half a contact). CORE refuses a line shorter than that length (`ShorterThanElectrode`), so `shallow` always lies above the most-proximal level. **Continuity** = fraction of the axis from contact 0 to 30 mm proximal whose transverse disc maximum (r = 1.5 mm) is ≥ 1800 HU.
3. **Selection:** TRAC/CORE when it traced the side; otherwise the metal axis, only if **corroborated** (continuity ≥ 0.9); otherwise the side fails with both reasons. `meta.side_source[side]` records `trac_core` or `metal_axis`; `meta.metal_axis` and `meta.side_notes` carry the evidence.

The RANSAC sampler is a fixed-seed SplitMix64 drawing two distinct indices per iteration (400 iterations, best inlier count wins, ties keep the earlier line). It does not reproduce a NumPy generator; the refined axis, not the RANSAC seed, determines the result.

**Image sampling** (metal on axis, continuity): every 0.5 mm along the axis, on-axis value plus the maximum over a transverse disc of radii 0.25 … 1.5 mm in 0.25 mm steps × 16 angles (π/8), never below the on-axis value; trilinear interpolation with indices clamped to the grid, so samples outside the volume take the nearest edge value.

Measured 2026-10-04 on the Lead-Tutor CTs: every lead's contacts and body read 2000–3070 HU along its axis, the 99.9th percentile of each whole volume (skull included) is 1450–1560 HU, and the three leads that a misaligned coregistration made look steep (§4.5) read continuity 0.95–1.00.

**4.5 Plausibility gates**

Native space, every side: span / pitch / radial offset / field of view (`check_side_geometry`), and **metal on axis** (median on-axis value ≥ 50 % of the disc maximum and, for CT, ≥ 300 HU). Template space, after the point warp: **brain mask** (template `brainmask.nii.gz`, 3 mm margin, when present; a contact passes when the mask voxel nearest to it is set, or when some set mask voxel centre lies within a **Euclidean** distance of 3 mm in world millimetres — not a per-axis voxel box) and **tilt** ≤ 55° from vertical.

**No tilt override.** The 55° refusal applies to every side, whichever path produced it (TRAC/CORE or metal axis). The refusal message names coregistration as a likely cause.

The tilt rule catches two failures. One is a trace that started on bone; continuous metal along the axis argues against that one. The other is a CT→anchor coregistration that left the CT rotated, and metal continuity says nothing about it: the lead is real metal, but it sits at the wrong angle and place.

A metal-backed override of this gate (2026-10-04) let three such coreg failures through on the Lead-Tutor cohort (`sub-29781`, `sub-15454`, `sub-33544`): 57–78° in template space, 9–12 mm from the expert markers on two of them. With the multi-start coregistration of `specs/03` §5.1, all three trace with TRAC/CORE at normal tilt and land within 1.4–3.2 mm. `meta.tilt_gate` is no longer written.

---

## 5. Fiducial → contacts (`resolve_coords`)

**Input:** `markers`, `Elspec`, `electrode` model template from `09` (`head_position`, `tail_position`, `x_position`, `y_position`, `coords_mm` in **model** frame).  
**Output:** `coords_mm`, 50-pt `trajectory`, updated `markers`.

**Math:** find **rigid** `X` (4×4) such that model fiducials map to world fiducials (least squares on 4 points); apply `X` to all canonical contact rows. Rebuild 50-pt line between anchors:

```
t_hat = (tail - head) / |tail - head|
a1 = head - 5 * t_hat
a2 = tail + t_hat * (contact_span * k)     # k = 1.5 if the model has more than 8 levels, else 2
trajectory = linspace(a1, a2, 50)
```

**Trajectory rule (normative, 2026-11-14).** `contact_span` is the catalogue array span of `09` §2.2 (not the head-to-tail distance and not a level-centre span), and `k` counts **levels** (entries of the level index list), not contacts. The same rule builds every trajectory written from 2026-11-14 on: native space, template space after the warp (from the warped head and tail), the `reference` block, manual correction and every export (`12` Part 1). A stored trajectory of an older file is rebuilt only when that side is converted (§8) or rewritten; until then exports carry it unchanged.

**Resize mode** (optional): scale model span to match measured spacing (3387/3389 use case in reference).

---

## 6. Save / load / hybrid spaces

**File:** `Subject.recon` path, single bundle **Reco** (see §8). Serialization: **MAT** legacy **or** JSON+HDF5 in Wyss DBS Localizer — **version field** required.

`**save_reconstruction` contract:**

- If `native_working=1` → write `native` first; if `hybridsave` → run **native→MNI** warp (`reconstruction_to_mni` in `00` coordinate chain), optional **AC-PC**, **LR swap check**, second save.  
- If `native_working=0` (MNI first) → write `mni` first; if `hybridsave` → **MNI→native**, then optional AC-PC, swap check.

**X/Y marker length:** after assign, renormalize to exactly `**lead_diameter/2`** from `head` on the perpendicular plane (reference behaviour).

**Point warp (Wyss DBS Localizer).** Transform tokens for native → template: `-t <from-<Space>_to-anchorNative composite>` (the inverse of the image transform; template → native uses the `from-anchorNative_to-<Space>` composite; names per `01` §7a). Only `antsApplyTransforms` is bundled, so points are mapped with a **coordinate image**: a float32 4-D image over the destination reference's world bounds ± 120 mm on an 8 mm grid whose voxel values are their own RAS world coordinates (sform code 2, qform code 0), resampled with `antsApplyTransforms -d 3 -e 3 -n Linear <tokens>` onto a 1 mm grid laid around the points (± 10 mm, origin floored to the grid), then sampled trilinearly at the points. All points of a reconstruction (contacts, trajectory, markers of both sides) go through one call; `trajvector` is recomputed from the warped `head`/`tail`. Points that land outside the coordinate image (all components 0) are `E_RECON_warp`, never returned.

**Template-space contacts from the warped markers (2026-10-15, reference-implementation behaviour).** After every write of a template-space side block (the `mni` block of the point warp above, the PaCER `mni` block, a manual-correction re-warp (§9a), an orientation rewrite of `x` / `y` (`06` §10.6), and the `reference` block mapped through the template bridge (§8)), its `coords_mm` are **recomputed from that block's markers** and the electrode model instead of being kept as individually mapped points:

- With the block's `head`, `tail`, `x`, `y` (x / y at radius `r = lead_diameter/2` perpendicular to `û = (tail − head)/|tail − head|`) and the catalogue model (`09`; model frame axis +z, head fiducial `z_h` = centre of the most distal level, tail fiducial `z_t` of `09` §2.2 (centre of the 4th level, or of the last level when the model has fewer than 4), model x / y fiducials at `(r, 0, z_h)` / `(0, r, z_h)`), `T(p) = head + [x̂ ŷ û] · diag(1, 1, s) · (p − head_model)`, `x̂ = (x − head)/r`, `ŷ = (y − head)/r`, `s = |tail − head| / (z_t − z_h)` (`s = 1` when the model span is 0 or not finite). This is the exact affine that maps the four model fiducials onto the four markers: rotation, translation and a stretch along the lead only.
- `coords_mm` = `T` applied to every catalogue contact centre, in catalogue contact order (one row per contact, segmented levels included), so the contacts lie exactly on the straight body that `08` §11 draws.
- The individually mapped contacts are kept as `coords_mm_warped` in the same block (same row order as the native `coords_mm`), for analyses that want the local deformation. A block that is written directly in template space (post-op MRI, §4.2; template-space manual correction) has `s = 1` up to rounding and gets the same treatment; there `coords_mm_warped` is omitted.
- Applies to every file, manually corrected or not (deliberate deviation from the reference implementation, which keeps the individually warped contacts for manually corrected files).
- Template `x` / `y` used here are the `06` §10.6 construction (perpendicular, radius `r`), built at every warp, so the frame is orthonormal by construction. Whenever the individually mapped contacts are at hand (a warp or bridge just ran), `coords_mm_warped` is written, also when the recompute falls back. `coords_mm_warped` always describes the **current** native contacts: a side whose native contacts are rebuilt later gets new ones — an orientation rewrite (`06` §10.6) sends that side's rebuilt native `coords_mm` through the subject point warp (this section) and stores the result as its `coords_mm_warped`; when no warp is available (missing transform, warp error, wrong point count) `coords_mm_warped` is **dropped** for those sides with `dbsloc: warning: orientation: individually mapped template contacts (coords_mm_warped) dropped for the re-oriented side(s) (<why>)`; a native-space manual correction (§9a) stores the edited side's individually warped contacts. A side that is not rebuilt keeps its existing `coords_mm_warped`.
- Degenerate markers keep the individually mapped contacts as `coords_mm` with the log line `dbsloc: warning: reconstruct: <block> side <k>: contacts kept as individually mapped points (…)`, never an error: any non-finite marker, `|tail − head|` ≈ 0, `|x̂·û|`, `|ŷ·û|` or `|x̂·ŷ|` above 0.25, or `|x − head|/r`, `|y − head|/r` outside [0.5, 2.0]. The electrode model is `props[<side>].elmodel`, else `meta.elmodel`. PaCER's native block has one row per level (as in the reference implementation); its template block then has one row per catalogue contact while `coords_mm_warped` keeps the per-level rows, until an orientation rewrite (`06` §10.6) or a manual correction (§9a) rebuilds the native block per contact (and with it `coords_mm_warped`, above).
- `trajectory`, `markers` and `trajvector` are unchanged by this step. The native block is not changed by it; only an orientation rewrite (`06` §10.6) and a manual correction (§9a) recompute native contacts from native markers.
- `meta.template_contacts = "from_markers"` records the rule; files without it predate it and keep their individually warped contacts until `reconstruct` (or a correction) runs again.
- Size of the change on the Lead-Tutor cohort: at most 0.17 mm along the lead and 0.23 mm across it (mean 0.09 mm along). Head / tail markers, and therefore Lead-Tutor scores, are unaffected.

---

## 7. PaCER (interface)

**Full algorithm spec:** `specs/10_pacer.md` (implementation contract for `crates/dbsloc-reconstruct::pacer`).  

**Pre:** `pacer` CT masks on disk, coreg CT.  
**Call:** in-tree Rust implementation (per `10_pacer.md` phases) or **stub** returning `(coords, trajectory, markers)` in **native** mm until the pipeline is complete.  
**Post:** the `mni` block comes from the §6 point warp of the native block, and the template-space gates of §4.5 (tilt, brain mask) apply as for TRAC/CORE.  
**Sides (Wyss DBS Localizer):** PaCER returns one result per accepted metal component, so a lead that cannot be placed does not discard the others. Leads are keyed by their distal centreline's offset from the head midline (mean *x* of −300 … 1800 HU voxels inside the CT's field of view, `10` §5a; native scanner *x* alone does not say which hemisphere), at least 2 mm off it; more than two components, two on one side, or one on the midline fails the stage. Any accepted lead that cannot be placed fails it too. PaCER runs only when `recon_method = pacer` (`auto` on CT is native-frame TRAC/CORE, §3); it never substitutes another algorithm and writes a unilateral reconstruction when exactly one lead is found and placed (`meta.sides_found`).  
**Errors:** `E_PACER` if engine fails or no CT.

---

## 8. Reconstruction file format (**Reco**)

Top-level (language-neutral):


| Field                            | Content                                                 |
| -------------------------------- | ------------------------------------------------------- |
| `props[side].elmodel`            | string from `09`                                        |
| `props[side].manually_corrected` | bool (one `props` entry per side present in the file)   |
| `native`                         | `{ coords_mm[], trajectory[], markers[], trajvector? }` |
| `mni`                            | same + optional `trajvector` in template                |
| `reference`                      | optional: `{ space, "1": side, "2": side }`, the `mni` block mapped into `MNI152NLin2009bAsym` |
| `scrf`                           | optional — same after brain-shift (native-class)        |
| `acpc`                           | optional: `coords_mm`, AC, PC, MSP vectors              |


`coords_mm[i]` = side index; `trajectory[i]` = `50×3`.  
`markers[side]`: `head`, `tail`, `x`, `y` (and optional z helpers).

**Side keys (normative):** side **1** is the **right** hemisphere (positive *x* in RAS) and side **2** the **left**, the order of `specs/09` §1 (`contactnames` right first, `etagenames{1}` = right group). In the JSON form the keys of `native`, `mni`, `scrf` and `props` are the strings `"1"` and `"2"`, and writers record `meta.side_keys = "1=right,2=left"`. Wyss DBS Localizer wrote the opposite order before 2026-10-04 (roadmap 2.18). **Readers resolve hemispheres, never raw keys:** a file declaring `meta.side_keys = "1=right,2=left"` is read by key; a file without it is read by mean *x* of each side (of two sides the larger is right; a single side goes by its sign); any other declared value, two sides at the same mean *x*, or more than two sides is an error. One helper per language applies this: `dbsloc.reconstruction.io.side_hemispheres` / `side_key_for` (Lead-Tutor export, montage, orientation MVP) and `crates/dbsloc-viz/src/reco_json.rs` (`dbs-localizer scene`, MNI slice). Lead-Tutor `leftElectrode` = right hemisphere = Lead-DBS side 1.

**Reference block (Wyss DBS Localizer):** `mni` is in the space of the normalization template (`meta.template_space`, `04` §3.1). When that space is not `MNI152NLin2009bAsym`, `reconstruct` adds a top-level `reference` object after `mni`: `"space": "MNI152NLin2009bAsym"` plus the sides `"1"` / `"2"` with every `coords_mm`, `trajectory` and marker mapped through the template space bridge (`04` §5.4a, `Bridge::points_to_reference`) and `trajvector` recomputed. Hemispheres of the block are resolved with the same rule (`side_hemispheres(reco, "reference")`). If the bridge cannot be built the file is written without `reference` and a `dbsloc: warning: reconstruct: no reference-space coordinates (…)` line is logged. Same-space runs carry no `reference` key. Consumers bound to the reference space (Lead-Tutor) read `reference` when `meta.template_space` differs and refuse a file without it.

**Reconstruction status (`meta.reconstruct`, normative 2026-10-16).** Every file `reconstruct` writes carries `meta.reconstruct = {status: {<k>: "traced" | "kept" | "failed"}, failed: {<k>: <reason>}, flags: {<k>: [<flag>, …]}, inputs: [<fingerprint>]}` with side keys of this section (`"1"` before `"2"`). `status` has an entry per side the run handled: `traced` (traced this run), `kept` (kept from a manual review, §9a), `failed` (left out, §4.4 *One lead*); a side PaCER did not find has no entry (`meta.sides_found` says which were found). `failed` is present only when a side failed. `flags` has an array for every side in the file (possibly empty); the defined flag is `vertical_marker` (`06` §10.6: at the native → template warp the side's native segment direction had no horizontal part, so its template `x` / `y` are the individually warped points). A kept side keeps its previous `flags` entry; a native-space manual correction (§9a) rewrites the `vertical_marker` entry of each edited side from its re-warp. The post-op MRI template path writes `traced` and empty flags. `inputs` is the input fingerprint of the run (the list of §9a *Per-side review*, taken when the file is written; `null` with a warning when it cannot be taken): it says with which coregistration and normalization the traced sides' coordinates were made. Every `reconstruct` run writes it, a PaCER import included; a manual correction never changes it.

**Tail fiducial convention (normative, 2026-11-14).** `markers.tail` is the model tail fiducial of `09` §2.2 (4th level), the convention of the reference implementation. Every file written from now on records `meta.tail_fiducial = "fourth_level"`. Files written before 2026-11-14 by this program placed the tail at the **last** level, which differs only for models with more than 4 levels. When a reader that uses the markers with the electrode model (resolve, template-space contacts, viewer, manual correction, group analyses, exports) loads a file without `meta.tail_fiducial` whose side model has more than 4 levels, it compares that side's native `|tail − head|` (the side's `mni` markers when the `native` block lacks that side or its head / tail, as for template-space reconstructions of §4.2) with the model's last-level span `z_last − z_h` and with `fiducial_span`; when the distance is nearer the last-level span, it converts that side in memory before any other use: in every block (`native`, `mni`, `reference`, `scrf`) `tail ← head + ((z_t − z_h) / (z_last − z_h)) · (tail − head)` and the trajectory is rebuilt with the rule of §5. `coords_mm`, `x` and `y` are kept. The conversion logs `dbsloc: warning: reconstruction: side <k>: tail marker moved from the last to the 4th contact level (file written before 2026-11-14)`; the next write stores the converted markers and the field. Files without the field whose distance is nearer `fiducial_span` (files of the reference implementation) are used unchanged.

**Orientation:** for a directional lead, `x` / `y` are an arbitrary in-plane basis until the orientation stage (`specs/06`) solves the segment azimuth; writers then set `meta.orientation` to an `unresolved: …` note so no consumer reads `x` as the segment direction.

**Ordering:** `coords_mm[0]` = distal … `coords_mm[N_c-1]` = proximal (standard 4-contact window).

**Native-frame CT output (Wyss DBS Localizer, `method = "trac_core_rust_native"`):** top-level keys `version`, `subj_id`, `method`, `mni`, `native`, `props`, `meta`; `meta` keys in order `native_input`, `nifti_aff_4x4`, `frame_rigid_4x4`, `frame_scale`, `frame_fit_rms_mm`, `lead_diameter_mm`, `eldist_mm`, `elmodel`, `n_contacts`, `level_z_mm`, `contact_level`, `is_directed`, `tracer` (`trac_core_rust`), `traced_space` (`native`), `side_keys`, `side_source` (`{left, right}`, the traced sides only), then when present `metal_axis` (`{side: {continuity, continuity_mm, threshold_hu, seeds: {ransac_voxels}}}`), `side_notes`, `orientation`, then `reconstruct` (above), and finally `hybridsave = "native -> mni"`.

**Interchange:** must round-trip to **0.01 mm** in fixtures.

**Provenance (`meta.software`, `meta.run_options`, `meta.settings`, normative 2026-10-22).** Every file `reconstruct` writes carries `meta.software = {product, version, git_hash, target_os, target_arch, written_at}` (the build that wrote the file; `written_at` RFC 3339 UTC; replaced on every reconstruct write, a PaCER import included), `meta.run_options` (the serialised `RunOptions` of the reconstruct stage without `el_model_error`; `el_model` is the resolved one) and `meta.settings` = `{recon_method (effective, from `method`), recon_method_requested, el_model, recon_bright_metal (false on the CT path), template, template_space, reg_backend: {preprocess, coregister, normalize} (from the method logs; `null` when a log is missing, `"ants"` when a log has no key), norm_isotropic_mm, norm_multispectral}`. A manual correction and the orientation stage keep `meta.software` and add `software: {version, git_hash}` to every record they write (`meta.manual_correction[<k>]`, `meta.orientation[<k>]`). The inputs fingerprint hashes input files, never the reconstruction file, so `written_at` does not change it.

**MATLAB twin (normative 2026-10-22).** Every write of the reconstruction JSON (reconstruct, orientation, manual correction) also writes `sub-<id>_desc-reconstruction.mat` beside it, per `specs/12` Part 1 (`reco` with `props`, `native`, `mni`; `dbsloc_provenance`); an export problem is a warning, never a stage failure. The `.mat` is derived from the JSON and is never read back by the pipeline.

---

## 9. Fiducial refinement (optional, CT)

**Models:** e.g. Medtronic 3387/3389 only.  
**Logic:** 2D resample along trajectory, profile peaks → head/tail, axial refine — `refine_fiducials` boolean in `Prefs`. **MRI:** no-op with warning.

---

## 9a. Manual correction (Wyss DBS Localizer, normative, 2026-10-14)

An expert may move a lead and set its roll after any reconstruction. The model is the one of the reference implementation's correction tool: the user edits only the **head** (distal) and **tail** (proximal) fiducials and the **roll** of one side; contacts, trajectory and ring-plane markers are always rebuilt from the electrode model (§5), never edited one by one.

**Entry points.** `dbsloc_pipeline::manual_correction::apply_manual_correction(sub, opts, request, cancel, log)` (library, used by the GUI *Refine localization* view of `08` §11.1.6), and `dbsloc reco correct <dataset_root> <subject> [--side right|left --head X,Y,Z --tail X,Y,Z [--roll DEG]] [--accept right|left]… [--source gui|cli]` (CLI; `--side`, `--head` and `--tail` go together, `--accept` is repeatable, at least one of the two is required, otherwise `E_RECON_manual: nothing to do: …`; `dbsloc reco show <dataset_root> <subject>` prints the markers, roll, `manually_corrected`, `review`, `roll_review` and edit space of each side as JSON, plus a top-level `warnings` list that holds the §8 tail conversion messages; the *Refine localization* view shows them in its status bar). Several sides may be corrected or accepted in one call; an untouched side is left byte-identical in every block. A request entry is a correction (`SideEdit {key, head, tail, roll_deg, accept: false}`) or an **accept** (`accept: true`: the side is marked as checked without an edit; `head`, `tail` and `roll_deg` are not read and no geometry, warp or block changes). A side given twice in one request is `E_RECON_manual: side <k> given twice`.

**Edit space.** The reconstruction file decides: a file with a `native` block (CT, §3; the anchor-native space of `03`) is edited in **native** millimetres, a file with only `mni` (post-op MRI template path) in **template** millimetres (`meta.template_space`). The request names its space (`native` | `mni`); a mismatch is `E_RECON_manual: <reason>`. The GUI shows the coregistered post-op CT (`coregistration/anat/…_CT`) as the backdrop of a native edit and the normalized post-op MR in use (`04` §5.1) for a template edit, so the image and the edited coordinates share one frame.

**Per side (key `"1"` right, `"2"` left, §8):**

1. **Electrode model** = `props[key].elmodel`, else `meta.elmodel`, else `RunOptions.el_model`; unresolved → `E_RECON_manual: no electrode model for side <key>`. `head == tail` (within 1e-6 mm) → `E_RECON_manual: head and tail coincide`.
2. **Roll (horizontal azimuth, decision 2026-10-22).** `t = unit(tail − head)`, `r = lead_diameter / 2`. The roll θ is the horizontal azimuth of `12` §11.5 / `06` §10.6: `x̃ = (cos θ, sin θ, 0)`, `x_dir = unit(x̃ − (x̃·t) t)`, `y_dir = t × x_dir` (θ = 0: segment A anterior; positive θ turns `y` toward −x, the same sign as the former axis rotation; the two agree on a vertical lead and differ by the tilt, 4.1° at 30° lateral tilt and θ = 45°). With `--roll DEG` θ = DEG; otherwise θ is **read back** from the edited block's previous markers, `θ = atan2(−d_x, d_y)` with `d = markers.y − markers.head` (negative when `y` lies on the +x side of `head`; θ = 0 when the block has no `y` or its horizontal part is shorter than 1e-6), so a head / tail move keeps the azimuth, not the projected previous `y`. When `x̃ ∥ t` (component ⟂ `t` shorter than 1e-6) the frame falls back to rotating `a_ref` about `t` by θ. Leads are written head = deep, tail = superior (`t_z > 0`), so the read-back is single-valued.
3. **Contacts** = §5 `resolve_fiducials([head, tail, head + r·x_dir, head + r·y_dir], model)`: rigid fit of the model fiducials, `coords_mm`, 50-point `trajectory`, `trajvector`, markers renormalised to `r`. The edited block (`native` or `mni`) is replaced by this geometry.
4. **Other spaces.** Native edit: the side's `mni` block is the §6 point warp of the new native block (same warper rule as `reconstruct`: Rust route on a wgpu record, ANTs otherwise, `--reg-backend` honoured), **except** `x` / `y`, which follow the `06` §10.6 azimuth construction from the native `y_dir` and the warped `head` / `tail` (flag `vertical_marker` as there). Then the `reference` block (§8) is rebuilt for the whole file when the template space is not the reference space, with the same `x` / `y` rule. Template edit: `mni` is the edited block, `reference` is rebuilt the same way; no native block is created.
5. **Record.** `props[key].manually_corrected = true`. `meta.manual_correction[key] = {review: "corrected", roll_review, at: <unix seconds, float>, source: "gui" | "cli", space: "native" | "mni", previous: {head, tail, x, y}, head_shift_mm, tail_shift_mm, roll_deg: <value or null>, inputs}` (`previous` = the markers of the edited block before this call; the object is keyed per side, a second correction of a side overwrites that side's entry and the first automatic markers are not kept; `roll_review` and `inputs` are defined under *Per-side review* below). `meta.orientation[key]` (`06` §10.6): when a roll was given it becomes `{status: "manual", method: "manual", roll_deg: DEG, candidates_deg: null, polar_angle_deg: null, marker_slice_mm: null, harmonic_amplitude: null, com_offset_mm: null, flags: []}`; when the roll was kept the previous object is left as it is (step 2 keeps the azimuth, so an azimuth `roll_deg` still holds; a pre-2026-10-22 axis-convention value is superseded by the markers, which `inspect` / `prepare` read back); a string value (`ORIENTATION_UNRESOLVED`) is left as it is.
6. **Plausibility** is advisory: the §4.5 tilt check and a contact-span check run on the new geometry and go to the returned `warnings`, never to an error. The expert's coordinates are authoritative.

**Per-side review (normative, 2026-10-16).** Each side carries its own review mark in `meta.manual_correction[<k>]`:

- `review`: `"corrected"` (a correction, step 5) or `"accepted"` (an accept). An accept of a side without a record writes `{review: "accepted", roll_review, at, source, space, inputs}`; an accept of a side that already has a record keeps it (a corrected side stays `corrected`, its `inputs` unchanged) and fills a null `roll_review` as below. `props[<k>].manually_corrected` stays `false` for an accepted side.
- **An accept needs current coordinates.** The side's coordinates were made with the record's `inputs` when the side has a record, else with `meta.reconstruct.inputs` (§8). An accept fails with `E_RECON_manual: side <k>: the inputs changed since the reconstruction (coregistration or normalization re-ran); run reconstruct before accepting the lead` when that fingerprint no longer matches a fresh one, and with `E_RECON_manual: side <k>: its coordinates carry no input fingerprint (the reconstruction predates it); run reconstruct before accepting the lead` when there is none. A correction rebuilds its side from the current inputs and records them for that side only; it never refreshes another side's record or `meta.reconstruct.inputs`.
- `roll_review`: `"set"` when a correction gave `--roll`; `"accepted"` when an accept confirmed a roll that was already determined, i.e. the side's electrode model is directed and `meta.orientation[<k>].status` is `resolved`, `ambiguous` or `manual`; otherwise `null` (the roll is left to the orientation stage). A correction without `--roll` keeps the side's previous `roll_review` (the roll is carried along by step 2).
- `inputs`: the input fingerprint at the time of the review, a list of `{path, exists, mtime_ns, size, sha256}` entries in the format of `00` §6 over the `reconstruct` lock inputs of `00` §6 plus the coregistered post-op CT (when the subject has one) and the native ← template inverse composite of the normalization (when present), deduplicated. When the fingerprint cannot be taken (an input exists but is unreadable) `inputs` is `null` with a warning, and a later `reconstruct` run drops the review.
- Readers: a record without `review` (written before 2026-10-16) counts as `corrected` with `roll_review = "set"` when its `roll_deg` is a number; `props[<k>].manually_corrected = true` without a record counts as `corrected`. `dbsloc_pipeline::manual_correction::{side_review, roll_review}` apply this rule; `dbsloc reco show` and `prepare_manual_correction` report both per side.

**File write.** The JSON is rewritten through `.partial-*` + rename (`outputs::write_replacing`); nothing else in the file changes (key order, untouched sides, `meta.*`). On any error before the rename the previous file is intact.

**Locks and dependents (`00` §6.1).** A correction is not a stage, but its effect on approvals is normative:

The stage locks are per subject and stage, the review is per side; the locks follow the sides as follows (2026-10-16; "present" = the sides of the edited block):

- A side **counts as reviewed** only while its record has a `review` and its `inputs` still match a fresh fingerprint; a review made before a re-run coregistration or normalization does not count.
- `locks/reconstruct.json` is written approved (`approval: true`, `method: "manual_correction"`, the stage's default input snapshot, `manually_corrected: <whether any reviewed side is corrected>`) only when **every present side** counts as reviewed, so a later run skips `reconstruct` and keeps the expert's leads; a re-run of an upstream stage (coreg, normalize, brainshift) still invalidates it as for any approval. Until then an approving `reconstruct` lock is invalidated (`invalidated_by: "manual_correction"`) and `reconstruct` stays unapproved; a later run re-runs it under *Reconstruct re-run* below.
- After a correction (any request with a non-accept entry) `vat`, `visualize` and `orientation` are invalidated as after a completed `reconstruct` (approval cleared with `invalidated_by: "manual_correction"`). An accept-only request changes no geometry and invalidates no dependent.
- `orientation`: its lock is written approved (`method: "manual_correction"`) when at least one present side has a directed electrode model and **every present directed side** counts as reviewed and has a `roll_review` (`set` or `accepted`), so the automatic estimate does not overwrite the expert's rolls; otherwise it stays invalidated (a correction) or as it was (an accept). Ring-only files never approve it (the stage skips them anyway, `06` §10.6).
- The log lines `dbsloc: manual correction: approvals invalidated: …` (`00` §6.1 format) and `dbsloc: manual correction: approved: …` list what changed.

**Reconstruct re-run (normative, 2026-10-16).** When `reconstruct` runs while the existing file has reviewed sides (`review` set), each reviewed side is **kept** from the existing file, untraced, when all of the following hold; otherwise its review is dropped with `dbsloc: warning: reconstruct: the manual review of the <left|right> lead (side <k>) is dropped: <why>; the lead is traced again`: the file's edit block (`native` for the CT path, `mni` for the post-op MRI path) has the side; the record's `space` matches that block; the record has `inputs` and they still match a fresh fingerprint of the same paths (`00` §6 comparison; a re-run coregistration or normalization changes them, so the corrected coordinates no longer fit); the side's `props[<k>].elmodel` names the same catalogue entry (`09`; names compare as the catalogue lookup does) as the run's electrode model when both are known. A kept side is logged with `dbsloc: note: reconstruct: the <left|right> lead (side <k>) is kept as reviewed in a manual correction (<review>)`. Sides not kept are traced (TRAC/CORE on the native CT or in template space, PaCER, or the PaCER import of §3) and the stage then copies, for each kept side, its `native`, `mni` and `reference` entries, `props[<k>]`, `meta.manual_correction[<k>]` (records of dropped sides are removed), `meta.orientation[<k>]` when it is an object (a string note becomes a per-side object, as in step 5), and its `meta.side_source` / `metal_axis` / `side_notes` entries, and sets `meta.reconstruct.status[<k>] = "kept"` with its previous `flags`. The `reference` block is rebuilt from `mni` as in §8 and the kept sides' previous `reference` entries are restored. When every side is kept nothing is traced and the file is rewritten from the existing one. On the post-op MRI template path a side that fails to trace next to a kept side is left out and recorded (`status: "failed"`, `failed[<k>]`, the §4.4 warning) as on the CT path; without kept sides any failure still fails the stage there. After the write, when every side in the file counts as reviewed, the stage writes the `reconstruct` lock approved as in *Locks and dependents* (this happens when the lock was lost or a side failed to trace next to kept sides); otherwise an approving `reconstruct` lock whose `method` is `manual_correction` is invalidated (`invalidated_by: "reconstruct"`, log `dbsloc: note: reconstruct: not every lead in the file is reviewed any more; the approval from the manual correction is withdrawn`), so a run that dropped reviews (inputs changed, *Rerun all selected steps*) does not leave the stage approved; an approval written by other means (`dbsloc lock approve`) is left as it is. Its dependents are invalidated by the completed stage as usual (`00` §6.1).

**Rerun all selected steps.** `RunOptions.overwrite_approved` (`--overwrite-approved`, GUI *Rerun all selected steps*) re-traces **every** lead and drops every review with `dbsloc: warning: reconstruct: Rerun all selected steps re-traces every lead; the manual review of <sides> is dropped`; the GUI warns on the Reconstruct card when any lead is corrected or accepted. On the `orientation` stage it estimates the roll of every directed side again and clears its `roll_review` (`06` §10.6).

**Return value.** `CorrectionReport {path, sides: [{key, space, review, head_shift_mm, tail_shift_mm, roll_deg}], warnings, invalidated: [stage names], approved: [stage names]}` (corrections first, then accepts; `review` = `corrected` or `accepted`, an accept reports shifts of 0 and `roll_deg: null`; `roll_deg` = the roll given in the request, `null` when kept; `invalidated` lists the stages whose approval was actually cleared, `reconstruct` included when an approval was revoked, `orientation` absent when it was re-approved; `approved` lists the locks written approved); the CLI prints it as JSON and exits 0, `E_RECON_manual` / BIDS errors exit 1 / 2. `file_edit_space(sub)` returns the file's edit space without needing the backdrop image (the CLI default for `--space`); `prepare_manual_correction` requires the backdrop image to exist.

**Details fixed by the implementation (2026-10-14).** A request without sides is `E_RECON_manual: no sides to correct`. When `meta.orientation` is the unresolved string and a roll is given, it becomes the per-side object with the edited side's `manual` entry; sides not edited keep no entry (the string is dropped). The `reference` block is rebuilt only when the file already had one; its untouched sides are restored byte-identical; for a template-space edit its `x` / `y` are the bridge-mapped markers (no azimuth construction, because the roll was set in template axes); when the block existed but the template file is missing it is dropped with the §8 warning. A native edit warps only when the file has an `mni` block. Plausibility warnings come from the §4.5 tilt check and the native side checks (span, pitch, shaft radius) without brain bounds. Invalidated locks carry `invalidated_by: "manual_correction"`.

**Errors.** `E_RECON_manual: <reason>` for: missing reconstruction file; a file without `version: 2`; an unknown side; a side absent from the edited block; wrong edit space; non-finite coordinates or roll; head / tail coincide; no electrode model; missing backdrop image (`prepare` only). `E_RECON_warp: <reason>` from step 4 (file untouched). Lock failures after the file was written are warnings (`lock: …`), never errors, because the correction itself is on disk.

**Acceptance.** (a) On a synthetic reconstruction, correcting with the automatic `head` / `tail` and no roll leaves `coords_mm` within 1e-6 mm and `x` / `y` within 1e-6 mm (the rebuild is idempotent). (b) Shifting `head` and `tail` by `(1, 2, 3)` mm shifts every contact by the same vector within 1e-6 mm and keeps the roll (`roll_from_anterior_deg`) within 1e-6°. (c) `--roll 30` gives `roll_from_anterior_deg(u, y − head) = 30 ± 1e-6` and `|x − head| = |y − head| = r`. (d) With an identity native → template map the `mni` block equals the native block (markers included) after a native edit. (e) The untouched side's JSON is byte-identical. (f) Locks: correcting both sides with a roll approves `reconstruct` and `orientation` and invalidates `vat` / `visualize`; correcting one of two sides leaves `reconstruct` and `orientation` unapproved (an approving `reconstruct` lock is invalidated); a correction without a roll plus an accept of the other side approves `reconstruct` and invalidates `orientation`. (g) A template-only file rejects `--space native`. (h) An accept changes no block, invalidates no dependent and records `review: "accepted"` (`roll_review: "accepted"` when the side's orientation is resolved, `null` when it is unresolved). (i) `reconstruct` with every side reviewed and unchanged inputs keeps both sides byte-identical, records `status: "kept"` and approves its lock; a changed fingerprint drops that side's review; `overwrite_approved` drops all and withdraws a `manual_correction` approval. (k) After a correction of side 1 and a changed input, an accept of either side is refused, a correction of side 2 leaves side 1's record untouched and does not approve `reconstruct`. (l) Electrode models are compared as catalogue entries (case, spacing and the catalogue file name are aliases). (j) `orientation` with every directed roll reviewed is skipped; with `overwrite_approved` it estimates again.

---

## 10. Edge cases

- Empty `sides` → no-op, success.  
- Missing norm volumes for TRAC → **E_RECON_input**.  
- PaCER on MRI → **E_PACER_mri**.  
- MNI↔native warp failure in hybrid → **E_RECON_warp** (partial file policy: implementation-defined, document).
- Manual correction (§9a) → **E_RECON_manual: <reason>**; the file is never half-written.

---

## 11. Acceptance criteria

- Synthetic BB-mask + plastic trajectory → **known** MNI contacts within **ε** (0.3 mm in-code target for unit test).  
- Rigid `resolve_coords` on tetra of points → sub-mm recovery on noiseless data.  
- `Reco` file round-trip.  
- `hybrid`: native point warped to MNI and back within coordinate spec tolerance.
- Brain-mask gate (§4.5): the margin is a Euclidean distance in mm — a contact 4.3 mm from the nearest mask voxel fails a 3 mm margin even when it lies within 3 mm of the mask along every axis separately (`plausibility::tests::brain_mask_margin_is_a_euclidean_distance`).

---

## 12. References

- TRAC/CORE: Horn / Lead-DBS methods papers.  
- PaCER: Husch et al. (2017) *NeuroImage: Clinical*.  
- Horn et al. (2019) *NeuroImage* pipeline.

---

*End spec 05.*