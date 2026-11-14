# Spec 03 — Coregistration (Wyss DBS Localizer)

**Status:** Phase B; §4 (Wyss DBS Localizer items), §5.1, §5.2 and §12.4a are normative and describe the implemented behaviour; the method tables of §5, the tone-map and the other Lead-DBS design text are not implemented (marked where they occur)  
**Depends on:** `01_bids.md`, `02_preprocessing.md`, `00_overview.md`

---

## 1. Purpose

Register all subject anatomies into one **anchor native** space: **fixed** = pre-op **anchor** MR; **moving** = other pre-op contrasts, post-op CT, and/or post-op MR. Output: **resliced** NIfTIs under `coregistration/anat/` with `space-anchorNative`, **linear transforms** on disk, **method JSON**, **QC PNGs**.

**Precoreg** (template header align) — see **§6** in `02_preprocessing.md` (same mathematical object, runs first in orchestration).

---

## 2. Coordinate frame

- **Anchor space** = 3D mm/world space of **anchor coreg NIfTI** after all linear steps; BIDS `space-anchorNative`.
- **Moving** volumes after coreg **must** match **fixed** affine **qform/sform** (conform step at end of each coreg).

---

## 3. Inputs / outputs

**Inputs:** `Subject.coreg.*` path slots, the coregistration method (fixed in code: `ants_rigid_multistart` for CT, `ants_rigid` for post-op MR and the T2w; there is no per-run method option), `Subject.coreg.anat.preop` (all in **preproc** space before coreg), post-op preproc paths, anchor modality key.

**Outputs:**

- Updated `Subject.coreg.anat.*.coreg` paths.
- **Transform files:** per pair, BIDS `forwardBaseName` / `inverseBaseName` + engine-specific extension (`.mat` ANTs/SPM, FLIRT `.mat`, etc.) — see `01_bids`.
- **Method log** `coregistration/log/sub-<id>_desc-coregmethod.json` (§12.4a): JSON with `method` (`ants_rigid_multistart` or `ants_rigid`), `engine`, `version` (application version), `custom`, `approval` and one record per image (`method`, `backend`, `image`, `transform`); no parameters hash.
- **QC:** PNG mosaics under `coregistration/checkreg/`.

---

## 4. Sub-pipelines and order

1. **Precoreg** — anchor → template reheader (§02). Skip if `coreg` file exists and not invalidated.
2. **Pre-op MR (non-anchor) → anchor** if `coreg_mr_enabled`. **Wyss DBS Localizer:** implemented for the **T2w** only, and run by the `normalize` stage when it uses the T2w as a second SyN channel (`specs/04` §5.2 steps 1–2): rigid, the argv of §5.2 with the centre-of-mass initialiser, no mask, no multi-start; moving = raw T2w, fixed = the anchor in use; outputs (names per `01` §7a) the T2w in anchor space `coregistration/anat/<raw T2w entities>_space-anchorNative_desc-preproc_T2w.nii.gz` and `coregistration/transformations/sub-<id>_ses-preop[_acq-<a>]_from-T2w_to-anchorNative_mode-image_xfm.mat`. Other pre-op contrasts are not coregistered.
3. **Post-op MR → anchor** if post-op is MRI and `coreg_mr_enabled` (method may differ from 2; reference allows ANTs-NonLinear fallback to linear). **Wyss DBS Localizer (normative):** run by the `coreg` stage for **every** post-op MR key the subject has (`ax_MRI`, `cor_MRI`, `sag_MRI`, in that order), whether or not a post-op CT is also present (the CT path of §5.1–5.2 runs too when it is). Rigid, the argv of the T2w coregistration (step 2: centre-of-mass initialiser, no mask, no multi-start), engine per `RunOptions::backend_for(Stage::Coregister)` (the `--coregister-backend` override, else `reg_backend`; `04` §5.4; `wgpu` = the in-process rigid MI used for the T2w). Moving = the raw post-op MR, fixed = the anchor in use (`02` §6.1 / §4.3). Outputs (staged, `00` §5.2), named per `01` §7a: the post-op MR in anchor space and its `from-<suffix>_to-anchorNative` transform; same empty-warp post-condition as the CT (`coreg: warped <key> is empty …`). **One commit set:** the CT outputs (§5.2), the outputs of every post-op MR key and the coreg method log are staged together and renamed into place only when every image of the stage succeeded; a failure in any of them (or a cancel) keeps every previous output, the previous method log and the lock (`00` §5.2). Result token: the CT token when a CT was coregistered, else `ok:coreg:<first coregistered MR image stem>`. `coreg` fails with `no postop CT or MR` only when neither exists. `normalize` then applies the SyN composite to every coregistered post-op image (CT and MR alike) onto the template grid, writing each post-op image in template space under its `01` §7a name, linear interpolation. `reconstruct` with post-op modality `MRI` uses the template-space TRAC/CORE path (`05` §3) on the normalized `ax_MRI`, else `cor_MRI`, else `sag_MRI`.
4. **Post-op CT → anchor** if post-op is CT and `coreg_ct_enabled`. The tone-mapped CT copy of the original design is not implemented.

**After any step materializes new coreg:** `gencheckreg(coreg)`.

**Locks:** `reg_locked(output_path)`; `**overwrite_approved`** bypass; reference warns: rerun CT coreg with overwrite → **delete** brainshift tree or warn; rerun MR coreg with overwrite → **segmask cleanup** + warn to rerun **normalization** — Wyss DBS Localizer **must** emit same **policy** or stricter (explicit invalidation graph in `00`).

---

## 5. Method dispatch (normative enums)

Implemented: only the method ids `ants_rigid_multistart` (CT, §5.1–5.2) and `ants_rigid` (post-op MR, T2w); the enums below are the original Lead-DBS design and none of their ids is accepted by the code.

**CT** (`coregpostopct` class):


| Method id     | Engine              |
| ------------- | ------------------- |
| `ants`        | ANTs linear         |
| `ants_multi`  | ANTs multiple runs  |
| `brainsfit`   | BRAINSFit           |
| `flirt`       | FSL FLIRT           |
| `flirt_multi` | FLIRT multiple runs |


**MR** (`coregimages` class):


| Method id              | Engine                                |
| ---------------------- | ------------------------------------- |
| `ants`                 | ANTs linear                           |
| `brainsfit`            | BRAINSFit                             |
| `flirt`                | FSL FLIRT                             |
| `flirt_bbr`            | FLIRT BBR                             |
| `hybrid_spm_ants`      | SPM coreg → ANTs                      |
| `hybrid_spm_brainsfit` | SPM → BRAINSFit                       |
| `hybrid_spm_flirt`     | SPM → FLIRT                           |
| `spm`                  | SPM only                              |
| `ants_nonlinear`       | (optional / dev) ANTs SyN short chain |


**Default MR:** `spm`. **Default CT:** `ants`.

**Discovery:** implement **either** static enum table **or** plugin registry (reference scans `ea_coregpostopct_*.m` for CT). Wyss DBS Localizer: **one** `CoregMethod` enum + metadata.

### 5.1 CT → anchor (`ants`): multi-start rigid initialisation (Wyss DBS Localizer, normative)

A single rigid MI run started from the intensity centre of mass converges to a wrong local optimum when the head is pitched differently in the CT and the MR. Measured on the Lead-Tutor cohort (2026-10-04): on `sub-33544`, `sub-15454` and `sub-29781` the CT skull ended up rotated against the T1 (frontal bone inside the brain, occipital bone outside), and the leads came out 57–78° from vertical in template space and ~10 mm too deep. The expert reconstructions have 20–40°. A coreg error alone produces that, so tilt-gate refusals on these subjects were correct. Clipping or shifting CT intensities does not fix it; starting from a different pitch does.

**Backend (`--reg-backend`, `--coregister-backend`).** `--reg-backend auto|wgpu|ants` (`specs/04` §5.4) is the default engine of every step and `--coregister-backend` overrides it for this stage alone; the selected engine is requested exactly as for `normalize`: `auto` (default) runs the in-process GPU path when a hardware adapter passes the device self-test and otherwise falls back to ANTs with a `reg_note`, `wgpu` requires the GPU (an explicit request errors without one), `ants` is the bundled `antsRegistration`. The `wgpu` path reuses the same multi-start (pitches `−40…+40°`, head centroids, fixed mask), the same coarse schedule and the same full masked rigid as the argv of §5.2, and writes `…_postopCT_to_<anchor>0GenericAffine.mat`, `Composite.h5` / `InverseComposite.h5` and `…_Warped.nii.gz` under the same names. Its coarse start score comes from the per-iteration optimiser trace, so it can differ from the ANTs stdout metric at the sampling level (accepted deviation of 2026-10-07: on near-ties the GPU and ANTs may pick different starts; acceptance counts such disagreements but does not fail on them); non-finite CT voxels (NaN, ±inf) are treated as 0 before registration, matching ITK. `_multistart.json` (step 5) gains `reg_backend`, `reg_requested`, `reg_note`, `reg_adapter`, `reg_timings` (coarse per start, full per level) and a per-start `metric_backend`; the in-process body (preprocessing, coarse multi-start, masked rigid, resampling) runs on a dedicated rayon pool of `ants_max_threads` workers when that option is set, the wording of `specs/04` §5.4 step 3 (`reg_backend::with_worker_threads`), and the run is cancellable between the coarse starts (the flag is polled before each one, like the ANTs closure) and within each registration call (per optimiser iteration, `specs/04` §5.4: the coarse starts, the masked rigid and the post-op MR rigid; a cancel inside a call stops the stage like a cancel between starts, is never a failed start and never falls back to ANTs under `auto`); the coreg lock follows the `normalize` backend rule (`auto` accepts either backend, an explicit request re-runs when the recorded backend of any image in the coreg method log differs — per-image `backend`, else the log's `engine`, else the multistart record's `reg_backend`; no key = `ants`); an approved coreg also re-runs when a post-op image the subject resolves now (logical key `CT` or `<acq>_MRI`, `01` §10) has no coregistered output under its current derivative name (`dbsloc: note: coreg: no coregistered output for <key>; the stage runs`, normative 2026-11-14), which covers a bundle key that changed after approval; and the `antsRegistration` prerequisite is skipped for `coreg` when the backend resolves to `wgpu` (`specs/04` §5.4). Measurement: `docs/gpu_coreg_report.md`.

**Algorithm** (`crates/dbsloc-pipeline/src/coreg_multistart.rs`, the only implementation since unification phase 2):

1. **Head centroids** (world RAS mm, then LPS for ITK = `diag(-1,-1,1)·RAS`). Fixed: mean position of finite voxels `> 0.15 × p99` (99th percentile of finite voxels). Moving CT: mean position of finite voxels `> −300 HU`. Non-finite voxels (NaN, ±inf) are ignored. If either mask is empty: error.
2. **Starts:** pitch `θ ∈ {−40, −20, 0, +20, +40}°`, i.e. rotations about the LPS *x* (left–right) axis. Each start is an ITK text transform (`#Insight Transform File V1.0`, `AffineTransform_double_3_3`) that maps fixed physical points to moving ones: `T(p) = R_x(θ)·(p − c_f) + c_m`. Write it as `Parameters:` = the 9 row-major entries of `R_x(θ)` followed by translation `c_m − c_f`, and `FixedParameters:` = `c_f`.
3. **Coarse rigid per start:** `Rigid[0.1]`, `MI[fixed,moving,1,32,Regular,0.25]`, `--convergence [200x100,1e-6,10]`, `--shrink-factors 8x4`, `--smoothing-sigmas 3x2vox`, the fixed `--random-seed`, `--verbose 1`, `--initial-moving-transform <start file>`, no warped image (`--output [<work prefix>]`). Any other flags the full stage uses (e.g. winsorize) apply here too. Score = metric value on the **last** `…DIAGNOSTIC,` iteration line on stdout (ANTs reports MI negated: **lower is better**). A start whose process fails, or whose stdout has no parseable metric, scores `+inf`; on the `wgpu` engine a registration error inside one start is a failed start too (`+inf`), not a stage failure. Each failed start is logged with its reason (`[coregister] start pitch <θ>°: failed: <reason>`, or `failed (no metric)`). If every start scores `+inf`: error `coreg multistart: every multi-start coarse rigid start failed; last failure: <reason of the last failed start>` (without the `; last failure` part when no start reported a reason). A cancel is not a failed start: it stops the stage.
4. **Full rigid:** the stage of §5 (4 levels) with `--initial-moving-transform <best start's 0GenericAffine.mat>` replacing the centre-of-mass initialiser, and a **fixed-image mask** `--masks [<fixed mask>,NULL]`. Ties in step 3 go to the start listed first in step 2. The run writes the outputs to the usual prefix (`…_postopCT_to_<anchor>0GenericAffine.mat`, `…_Warped.nii.gz`); the external tool receives short `.partial-*` work names in the target directory and the results are committed under the `01` §7a names (`00` §5.2).
   - **Fixed mask** = voxels of the fixed image that are finite and `≠ 0`, eroded by 2 voxels: a voxel stays only if every voxel within ±2 along each of the three grid axes is also in the mask, and voxels outside the volume count as outside the mask. Write it as `uint8` NIfTI with the fixed image's affine, at `coregistration/log/multistart/fixed_mask.nii.gz` (the work directory of step 5). If the mask is empty: error.
   - Why: defaced MR anchors carry exact zeros where the face was cut away (15–31 % of every Lead-Tutor T1), and the CT still shows the face. Unmasked, MI also scores how CT air lines up with that blank region.
   - The coarse runs of step 3 stay **unmasked**; that is the combination measured below.
5. **Record** `coregistration/log/sub-<id>_ses-postop_desc-multistart_CT.json`: `{"starts": [{"pitch_deg", "metric"}...], "chosen_pitch_deg", "fixed_centroid_lps", "moving_centroid_lps", "coarse_schedule", "fixed_mask"}`. Scores of `+inf` are written as `null`. Work files (start transforms, coarse outputs, fixed mask) go in `coregistration/log/multistart/`.

**Measured** (Lead-Tutor, 2026-10-04). Score = mean of head and tail error against the expert markers, per lead, in mm, +x lead / −x lead.

| Subject | Old single start | Multi-start | Multi-start + masked full |
| ------- | ---------------- | ----------- | ------------------------- |
| 15454 | 12.36 / 9.04 | 3.29 / 4.37 | 1.90 / 3.23 |
| 29781 | 1.78 / 2.38 | 0.85 / 1.68 | 1.53 / 1.41 |
| 33544 | 11.92 / 12.33 | 1.37 / 1.90 | 1.45 / 1.93 |
| 39468 | 0.83 / 0.90 | 0.84 / 0.49 | 0.83 / 0.52 |
| 57245 | 0.64 / 1.67 | 1.47 / 1.31 | 1.21 / 1.48 |
| 76325 | 1.75 / 1.28 | 1.73 / 1.81 | 1.88 / 1.64 |
| 78754 | 0.92 / 1.12 | 0.99 / 1.06 | 0.77 / 1.13 |
| 80206 | 1.00 / 1.03 | 0.60 / 1.20 | 0.95 / 1.09 |
| 84257 | 1.11 / 2.81 | 0.79 / 1.42 | 0.74 / 1.46 |
| 93127 | 1.10 / 1.37 | 1.12 / 1.37 | 1.07 / 1.45 |
| **mean** | **3.37** | **1.48** | **1.38** |

Coarse scores on `sub-33544`: −0.364, −0.348, −0.383, **−0.544**, −0.544 for −40…+40°. The 0° start is the old behaviour. Old single-start results on 29781 / 15454 / 33544 used the metal-axis tilt override that `specs/05` §4.5 has since removed. A mask alone, with a single 0° start, still fails on 33544 (8.5–9.9 mm). Cost: five coarse runs take ~25 s on 4 threads, on top of the full run.

### 5.2 CT → anchor (`ants`): argument vectors (Wyss DBS Localizer, normative)

One implementation: `crates/dbsloc-pipeline/src/ants_argv.rs` (`build_ants_ct_coreg_argv`, `build_ants_ct_coarse_argv`), used by `dbsloc run`, the GUI and `dbsloc_pipeline_runner` alike (unification phase 2, 2026-10-05). The flags are the ones validated on the Lead-Tutor cohort (table above), in this order:

- **Full rigid run:** `--dimensionality 3 --output [<pfx>,<pfx>_Warped.nii.gz] --initial-moving-transform <best start .mat | [fixed,moving,1]> --transform Rigid[0.1] --metric MI[fixed,moving,1,32,Regular,0.25] --convergence [1000x500x250x100,1e-6,10] --shrink-factors 8x4x2x1 --smoothing-sigmas 3x2x1x0vox [--masks [<fixed mask>,NULL]] --random-seed 20260910`, then the `--ants-…` pass-through tokens. After a successful full run the stage writes `Composite.h5` / `InverseComposite.h5` from the resulting `0GenericAffine.mat` with the same writer as the `wgpu` path (normative 2026-11-14), so both backends leave the same files and a re-run on the other backend never removes a file the previous run made.
- **Coarse start** (§5.1 step 3): `--dimensionality 3 --output [<work pfx>] --initial-moving-transform <start .txt> --transform Rigid[0.1] --metric MI[fixed,moving,1,32,Regular,0.25] --convergence [200x100,1e-6,10] --shrink-factors 8x4 --smoothing-sigmas 3x2vox --random-seed 20260910 --verbose 1`, then the same pass-through tokens.
- **Not passed:** `--winsorize-image-intensities`, `--use-histogram-matching` (ANTs default: off), `--float` (default: double precision), `--interpolation` (default: `Linear`). The last two are ANTs defaults (bundled `antsRegistration --help`), so dropping them changes nothing; winsorizing was never part of the cohort-validated recipe.
- **Post-condition:** a `_Warped.nii.gz` whose voxels are all non-finite or all `|v| < 1e-6` fails the stage (`coreg: warped CT is empty …`): the headers put the CT outside the anchor's field of view.
- Moving image: the raw post-op CT (`postop_anat["CT"]`); fixed image: the anchor of `specs/02` §4.3 (N4 output when present). Outputs are committed under the `01` §7a names (CT in anchor space, `from-CT_to-anchorNative` transform). Result token `ok:coreg:<coregistered CT image stem>`.

The argv snapshot tests in `ants_argv.rs` pin these vectors against frozen snapshots.

---

## 6. Conform step (post-condition)

After each successful coregistration:

- Open **fixed** and **output** NIfTIs; if world affines differ: **reslice** output to fixed grid (trilinear), normalize header so `fixed.mat == output.mat`.

---

## 7. Applying a saved transform (utility)

`apply_coreg(moving_path, ref_path, out_path, transform_path, interpolation)`:

- **Detect** transform type from file extension and/or JSON sidecar: SPM `.mat`, FSL `.mat` + warp if any, ANTs composite, BIDS `desc-` naming per `01`.
- **Reslice** moving into ref space with chosen interp: **linear** for continuous, **nearest** for labels.

**Used** by PaCER mask path, brainshift, etc.

---

## 8. QC

**Checkreg** PNGs: one panel per (fixed, moving) or mosaic; must list **all** channel pairs the pipeline touched. Deterministic **filename** pattern under `01_bids` checkreg map.

---

## 9. Edge cases

- Missing modality file → **skip** with log, not always fatal.  
- **CT** with extreme FOV: method-specific caps (left to engine).  
- **Bilateral** or multi-run: `*_multiple` methods — same transform fusion rules as reference (iterative or best-of; **spec implementation** in Phase C, not here — flag `multi_run_policy`).

---

## 10. Acceptance criteria

- ANTs: one fixture, output affine matches fixed (≤ float ε). FLIRT and SPM are not implemented.  
- Lock prevents overwrite without `overwrite_approved`.  
- Conform: RMSE in mm at **identity** voxels = 0.  
- JSON `coreg.log.method` round-trip.

---

## 11. References

- Jenkinson & Smith 2001/2002 (FLIRT), Greve & Fristl 2009 (BBR)  
- Avants 2008; Johnson 2007 BRAINSFit; Friston 2007 (SPM)  
- User guide: [https://netstim.gitbook.io/leaddbs](https://netstim.gitbook.io/leaddbs)

---

## 12. Addendum — checkreg viewer (GUI + batch PNGs)

**Normative detail** for interactive check-registration tool, batch checkreg figure generation, scrollable pair viewers (behavioral contract derived from reference — not GPL source import). **Normalization** uses same viewer branches (see `specs/04_normalization.md` §18). **D2/D3 Elvis GUI** reference: `specs/08_visualization.md` §11.

**Interactive volume viewer (NIfTI, window/level, zoom):** `specs/08_addendum_interactive_volume_viewer.md` (extends `dbs-localizer checkreg` / manifest JSON).

**Parent specs:** this document; `specs/04_normalization.md`  
**Behaviour reference (GPL):** the Lead-DBS check-registration functions `ea_checkreg`, `ea_gencheckregfigs`, `ea_gencheckregpair`, `ea_show_coregistration`, `ea_show_normalization` and `ea_imshowpair`; this section records their observable behaviour only.

### 12.1 Figure generation (batch, no window): `ea_gencheckregfigs(options, type)`

**Entry:** Pipeline after coreg / norm / brainshift, or “Tools / generate checkreg” style (`options.gencheckreg` in reference).

**`type`:** Cell or char: subset of `{'coreg','norm','brainshift'}`. Default in reference: both `coreg` and `norm` if single-arg overload.

#### 12.1.1 `type = 'coreg'`

1. `mkdir` `…/coregistration/checkreg/`.
2. **Fixed (anchor) image:** `options.subj.coreg.anat.preop.(AnchorModality)`.
3. **Moving images:** all entries of `subj.coreg.anat.preop` **except** removing the anchor key; plus `subj.coreg.anat.postop` with **CT stripped** (tone-mapped CT used elsewhere, not raw CT in this struct) or all postop MR fields — see reference struct logic.
4. **Output PNG path per pair:** parallel lists `coregImage` / `checkregFigure` from `subj.coreg.checkreg.preop` / `postop` struct cells (preop non-anchor + postop modalities). Empty branch handling if only preop or only postop.
5. For each `i` with `isfile(coregImage{i})`: **`ea_gencheckregpair(moving, fixed, outPngPath)`** (see §12.2).

#### 12.1.2 `type = 'norm'`

1. `mkdir` `…/normalization/checkreg/`.
2. **Fixed = template T1w:** `[ea_space, primarytemplate, '.nii']` in reference (template package path + stem).
3. **Moving images:** all `subj.norm.anat.preop` + postop (CT stripped analogous to coreg).
4. **Output paths:** `subj.norm.checkreg.preop` / `postop` struct cells, aligned with `normImage` list.
5. Loop `ea_gencheckregpair` as for coreg.

#### 12.1.3 `type = 'brainshift'`

1. `mkdir` `…/brainshift/checkreg/`.
2. **Fixed:** `subj.brainshift.anat.anchor`.
3. Two pairs if files exist: (a) `moving` = `brainshift.anat.moving` → `checkreg.standard`, (b) `scrf` → `checkreg.scrf`, with **`preproc = false`** passed to `ea_gencheckregpair` in reference.

---

### 12.2 Single-pair PNG: `ea_gencheckregpair(moving, fixed, outfn, preproc, lbound, ubound)` (visual contract)

**Parameters (reference defaults):** `preproc` default true; `lbound=0.2`, `ubound=99.7` (percentile ranks on sorted nonzero intensities).

**When `preproc==true`:** Intensity windowing on **moving** volume → temp NIfTI; then **FSL `slicer`** invoked with:

- Args: `preprocessed_moving`, `fixed`, optional `-e 0.05` (edge on **fixed**), `-i Lbound Ubound` intensity window, **`-a outfn`**.

**Effect:** **Greyscale = moving (registered)**; **red wireframe edges = from fixed**; montage of axial slices in one PNG. Misalignment shows as offset edges.

**When `slicer` fails:** GUI path falls back to static placeholder `helpers/gui/coreg_msg.png` (path in Lead-DBS only — Wyss DBS Localizer may use its own error image).

**Wyss DBS Localizer note:** FSL is optional; a native implementation must **match the visual role** (moving intensity + edge overlay of fixed) even if not pixel-identical to FSL.

---

### 12.3 Interactive viewer: `ea_checkreg` (GUIDE figure, singleton = off in code)

**Window title (opening):** `subjId + ': Check Coregistration'`.

**Appdata (figure):**

- `options` — full pipeline options.
- `checkregImages` — **ordered** cell list of **absolute** NIfTI paths to review (see §12.3.1).
- `b0restanchor` — parallel cell; for rest/b0 rows, the anchor side filename; else `[]`.
- `activevolume` — 1-based index into `checkregImages`.

**If `checkregImages` empty:** set base workspace `checkregempty=1`, **close** figure, return.

**Opening flags:** `options.overwriteapproved = 0` for session.

#### 12.3.1 Build order of `checkregImages` (only files that exist and are not “reg locked” in reference)

1. All **coreg preop** non-anchor volumes (all modalities in `coreg.anat.preop` except first/anchor — implemented as `struct2cell` **excluding duplicate anchor** in reference).
2. **Postop:** if modality CT → one path `tonemapCT` (the tone-mapped CT is not implemented); if MRI → all `struct2cell(postop)`.
3. **Norm anchor in template space:** `norm.anat.preop.(AnchorModality)`.
4. **Brain shift SCRF** image if field present: `brainshift.anat.scrf`.
5. **fMRI rest / b0:** optional rows from prefs (`rest_searchstring`, `b0` filename patterns) with extra locking rules.

**Filter:** `ea_reglocked(options, f) != 1` and `isfile(f)`.

#### 12.3.2 Per-frame UI refresh: `ea_mrcview` (synthetic name in reference)

**Labels depend on current volume** (`currvol = checkregImages{activevolume}`):

| Condition | `anchortxt` | `coregresultstxt` | `recomputebutn` label | `coregmrmethod` tooltip |
|-----------|-------------|-------------------|------------------------|---------------------------|
| Current = normalized anchor | `Template (red wires):` | `Normalization results` | `(Re-) compute normalization using...` | choose normalization method |
| Else (coreg path) | `Anchor modality (red wires):` | `Coregistration results` | `(Re-) compute coregistration using...` | choose coreg (MR) method |

**Last slide:** `approvebutn` / `disapprovebutn` show **“Approve & Close”** / **“Disapprove & Close”**. **Otherwise:** **“Approve & Next >>”** / **“Disapprove & Next >>”**.

**Special case:** If current is **brainshift SCRF** and it is the **last** image in the list, reference **closes** and launches **`ea_subcorticalrefine(options)`** (subcortical refine GUI) from within view refresh (see `ea_mrcview` brainshift branch).

**Controls:**

| Control (tag in `.m`) | Role |
|----------------------|------|
| `patientname` | Shows `subjId` (read-only). |
| `imgfn` | Shows relative path to current **checkreg PNG**; tooltip = full path. |
| `depvolume` | Shows current **moving** volume basename. |
| `anchormod` | Anchor modality string (e.g. T1w); for substitute flow, user-edited string. |
| `substitute` | Popup: alternative anchors (quoted modality names); selection updates `anchormod` (regex parse). |
| `coregmrmethod` | Dropdown: **coreg** or **norm** method string list from prefs (`ea_init_normpopup` for norm view). |
| `normsettings` | Button, **Visible=off** by default; **on** for norm view — opens normalization settings via appdata `normsettingsfunc`. |
| `recomputebutn` | Re-run the relevant stage with `options.overwriteapproved=1` (see §12.3.3). |
| `approvebutn` | Approve + advance or close (see §12.3.4). |
| `disapprovebutn` | Disapprove + advance or close (see §12.3.5). |
| `back` | Decrement `activevolume` if &gt; 1; refresh view. |
| `openviewer` | **Scroll viewer:** if current is norm → `ea_show_normalization` else `ea_show_coregistration` + moving/fixed/tag (§12.4). |
| `refreshview` | Regenerate current PNG from volumes (`ea_gencheckregfigs('norm')` or `ea_gencheckregpair` for current pair). |
| `checkatl` | Hidden in norm branch; if visible, calls `ea_checkstructures` (atlas/structure check — separate tool). |
| `openpatientdir` | `ea_opendir(subjDir)`. |
| `methodbutton` | GUIDE placeholder (callback empty in distributed snippet) — treat as **unused** unless verified in `.fig`. |

**Figure handle:** `handles.leadfigure` — `ea_busyaction('on'|'off', leadfigure, 'coreg')` for modal busy overlay during long ops.

#### 12.3.3 `recomputebutn` (re-compute) — behavior summary

Sets `options.overwriteapproved = 1`. Branches (simplified; full logic in reference):

- **Current volume is normalized anchor:** delete norm (and postop norm) products; set `options.normalize.method` from dropdown; **`ea_normalize(options)`**; `ea_gencheckregfigs('norm')`; set `norm.log.method` `json.approval = 0`; `ea_segmask_cleanup`; warn if `brainshiftDir` exists.
- **CT postop tonemap + current is that volume:** set `options.coregct.method`, anchor from `anchormod`; **`ea_coregpostopct`**, regenerate pair PNG; `coreg.log.method` `json.approval.CT = 0`; brainshift warn.
- **FA special case** — rare path with `fa2anat` / `ea_coregimages` (see reference).
- **Default MR** — set `options.coregmr.method`, possibly `options.subj.AnchorModality` from `anchormod`; session preop vs postop → `ea_coregpreopmr` or `ea_coregpostopmr` or b0/rest special transform moves; `ea_gencheckregpair` for the active pair; segmask / brainshift / norm warnings as appropriate.

#### 12.3.4 `approvebutn` — JSON side effects

- **If current = normalized preop anchor:** load `norm.log.method` JSON. If **all** non-anchor preop coreg files are reg-locked, `approval = 1`; else warn and `approval = 0.5` (multispectral norm caveat). Save JSON.
- **If current = postop tonemap CT:** `coreg.log.method` `json.approval.CT = 1`.
- **Else (modality from path):** `json.approval.(modality) = 1` on `coreg.log.method`; if approving preop coreg after norm was already fully approved, may downgrade norm `approval` to `0.5` with warning (multispectral).

**Then:** macOS `xattr` tweak on current volume file (Finder color label — **optional** in Wyss DBS Localizer). Advance `activevolume` or **close** if at end. If second-to-last is brainshift **scrf**, reference **closes and opens** `ea_subcorticalrefine`.

#### 12.3.5 `disapprovebutn` — JSON side effects

- **Norm:** `approval` 0 or 0.5 depending on all preop locked; save `norm.log.method`.
- **CT:** `json.approval.CT = 0`.
- **Modality keys:** set `json.approval.(modality) = 0`; if undoing preop after norm was 1, may set norm to `0.5` with warning.

**Then:** optional `ea_cleandownstream` for b0/rest. Advance or close; **no** subcortical auto-launch on disapprove in the snippet (only approve path hits brainshift end).

#### 12.3.6 `openviewer` → detailed scroll viewer

- Norm: **`ea_show_normalization(options)`** (builds 4D tensor, calls `ea_imshowpair` with `callingfunction` normalization).  
- Coreg: set `options.moving`, `options.fixed` (anchor or `b0restanchor` for rest rows), `options.tag` → **`ea_show_coregistration`**.

---

### 12.4 `ea_show_coregistration` / `ea_show_normalization` (data prep for `ea_imshowpair`)

**Coregistration:** z-score + clamp moving/fixed; build **3 RGB blend** channels: `0.1F+0.9M`, `0.4F+0.6M`, `0.9F+0.1M`; `img = cat(4, fixed, moving, joint)` as **4D** single‑precision; tag string for title.

**Normalization:** analogous prep; **`ea_imshowpair`** uses **`wiresIX=3`** (one joint channel) vs coreg **`3:5`** (three joint channels) — see `ea_imshowpair` `callingfunction` switch.

**Figure:** `ea_imshowpair`: **maximized** black `figure`, **no** menubar; **drag‑and‑drop** bound; name = `subjId, title` joined.

---

### 12.4a Wyss DBS Localizer pipeline gate (normative for Wyss DBS Localizer)

Implemented once, in Rust (`crates/dbsloc-pipeline/src/checkreg.rs`), drawing the PNGs in-process (`dbsloc_checkreg::gencheckregpair_png`, `PngWriter::Viz`, the default in every front end) and starting the viewer as the `checkreg` mode of the one GUI program, `dbs-localizer checkreg --manifest <manifest>`. There are no separate helper binaries (2026-10-23). Runs after a **successful** `coreg`, `normalize` or (with `DBSLOC_SCRF_ANTS=1`) `brainshift` stage when `RunOptions.checkreg` is true (CLI default; `--no-checkreg` or `DBSLOC_OPEN_CHECKREG=0` turns it off, `DBSLOC_OPEN_CHECKREG=1` forces it on). A gate failure fails the stage it follows.

1. **Slides** (images taken from the `Subject` bundles, `01` §7a; PNG = `<image stem>.png`). coreg: every coregistered post-op image (modality = its bundle key, `CT`, `ax_MRI`, …) and every coregistered pre-op image (modality = its suffix, e.g. `T2w`); fixed = the anchor (`specs/02` §4.3) in both cases, as in §12.1.1 (until 2026-10-05 a pre-op pair was shown against the raw `<from>` image itself, i.e. against a different space). norm: the normalized anchor (modality = anchor), then, when the normalization method log (`specs/04` §5.2 step 6) lists `T2w` and its coregistered T2w exists, that image vs the anchor (modality `T2w_to_<anchor>`), then every normalized post-op image (modality `postop<key>`); **fixed image per slide:** the SyN `_Warped` slide lives on the registration grid, so its fixed image is `normalization/work/template_is<mm>mm.nii.gz` (recreated by the gate, removed after an approved gate) when `norm_isotropic_mm` is set (default 1.0 mm, `04` §5.1 step 2; `null` = full template grid) and that file exists, else the template; the post-op slides are written on the **original template grid** (`04` §5.1 step 4), so its fixed image is always the template itself (the viewer tool rejects a pair whose dimensions differ, `CheckregShape`, and the gate would fail the stage after its outputs were committed). Both backends remove `normalization/work/` at stage end and the gate recreates the grid template it needs; the T2w → anchor slide is on the anchor grid and uses the anchor. brainshift: moving vs anchor (`pre_scrf`) and the SCRF output vs anchor (`post_scrf`), both with `--no-preproc`.
2. **PNGs** (drawn in-process by `dbsloc_checkreg::gencheckregpair_png`; `dbsloc viz checkreg gen` calls the same function) at `<stage root>/checkreg/<moving name without .nii.gz>.png` (`<stage root>` = `coregistration`, `normalization`, `brainshift`), manifest `locks/checkreg_manifest.json` = `{manifest_version: 1, subj_id, active: 0, subj_dir, slides: [{checkreg_png, moving, fixed, branch, modality, method_log}]}` with absolute paths.
3. **Review hook (in-process front ends).** When the run carries a review hook, the gate calls it instead of the viewer (no display check, no binary) on the pipeline thread with `ReviewRequest { stage, branch, manifest, slides, subj_id, method_log }`; the hook returns after the reviewer decided every slide (`Decided`), on cancellation (`Cancelled` → `cancelled by user`) or on failure (`Failed(msg)` → `<stage>: review failed: <msg>`). After `Decided`, step 4 decides as for the viewer. Otherwise: **Viewer** `dbs-localizer checkreg --manifest <manifest>` only when a display is available and the run is interactive (not `--headless` / `DBSLOC_HEADLESS=1`) and `DBSLOC_CHECKREG_SKIP_NO_DISPLAY` is unset. Interactive with no display is an error naming `--headless` and `DBSLOC_CHECKREG_SKIP_NO_DISPLAY=1`; headless with no display logs that QC passes only with a pre-approved method log. Interactive with a display but no `dbs-localizer` program fails with `<stage>: dbs-localizer not found; …`. The viewer runs with stdin and stdout on the null device; the pipeline polls it and kills it when the run is cancelled (stage message `cancelled by user`). A non-zero viewer exit fails the stage with `<stage>: checkreg viewer exited <code>`; a viewer that cannot be started, with `<stage>: checkreg viewer failed to start …`. The PNG and the manifest are written as `.partial-<name>` and renamed (`00` §5.2), and so is the method log the viewer writes on approval. Every path is unit-tested with the display verdict, the skip variable and the viewer injected (`checkreg.rs` tests; fake viewer = a shell exiting with a chosen code).
4. **Gate:** the method log (`coregistration/log/sub-<id>_desc-coregmethod.json`, `normalization/log/sub-<id>_desc-normmethod.json`, `brainshift/log/sub-<id>_desc-brainshiftmethod.json`) must approve: `approval` (or `approved`) `true` / `1`, or a map whose non-null values contain no `0`/`false` and at least one `1`/`true`. Otherwise `<stage>: qc not approved — approve in the viewer or edit <log> (field approval)`.
5. The `dbs-localizer` program (viewer modes `checkreg` and `scene`) resolves as `$DBSLOC_GUI_BIN` (path of the executable), then beside the running executable, then `PATH`, then `target/{release,debug}` under `$CARGO_TARGET_DIR` and the Wyss DBS Localizer tree (`dbsloc_pipeline::paths::gui_binary`). **GUI (2026-10-14):** setting *Review registrations during the run* (`StageSettings.review_registrations`, default on). On: `checkreg = true`, `headless = true`, and the run carries an in-process **review hook** (`run_pipeline_channel_with_review`, `ReviewSetup`) plus an in-process PNG writer (`PngWriter::InProcess`), so the app never starts a separate viewer process. Off: `checkreg = false` as before (no slides, no gate); a finished coreg / normalize can still be reviewed from its step card (`prepare_review` builds the slides and manifest without gating, `finish_review` removes `normalization/work/` after an approved norm review). The review is the `dbsloc_viz::checkreg_app::CheckregReviewPanel` in the app's *Registration review* tab (pop-out like the 3D Viewer); the app writes each decision with `apply_approval` (fields of §12.3.4–12.3.5 unchanged). **Several slides, one scalar field (norm, brainshift):** the decisions of all slides that share the scalar `approval` are combined: `1` only when every one of them is approved, `0` as soon as one is rejected (`dbsloc_pipeline::apply_decisions`; the `dbs-localizer checkreg` mode applies the same rule and writes nothing until every slide is decided or one is rejected). Before 2026-10-14 the last decision overwrote the earlier ones. The `0.5` value of §12.3.5 is not written by Wyss DBS Localizer. **Decision marker:** every reviewer decision (`apply_approval`, `apply_decisions`, the standalone viewer) also records top-level `reviewed`: `true` for the scalar branches (norm, brainshift), and for coreg the list of decided modality keys (`"reviewed": ["CT"]`; a legacy `true` on a coreg log counts as every key), so an interrupted coreg review leaves the undecided images *not reviewed*; stage bodies write a fresh log without it and the invalidation of `specs/00` §6.1 removes it, because a stage-written `approval: 0` means *not reviewed yet*, not *rejected*. Review state for the GUI: *approved* = the gate rule of step 4; *rejected* = not approved and a reviewed value (the scalar, or a listed coreg key) is exactly `0` / `false`; otherwise *not reviewed* (a value such as `0.5` is never *rejected*). A scalar log cannot say which slide was rejected, so a later review of a rejected norm asks for every slide again. A GUI approval never writes the stage lock `locks/<stage>.json`: QC approval and lock approval stay distinct (`specs/00` §6).

**Review modes and batch runs (2026-10-22, `specs/13` §11).** The GUI setting becomes `StageSettings.review_mode` = *During the run* (`DuringRun`, the former `review_registrations: true`, default), *After all subjects* (`AfterAll`) or *Off* (the former `false`); old state files load. *During the run* pauses every subject at its gate as above. *After all subjects* runs the queue with the gate on and no hook (each subject stops at an unapproved gate with the `awaiting_qc` outcome, `00` §5.2) and then opens the pending reviews one by one in the *Registration review* tab: approve writes the method-log approval and queues that subject's remaining steps, reject marks the step *needs rework*; *Skip this review* and *End the batch* are offered. A single-subject run under *After all subjects* reviews during the run. The CLI equivalents are the default (viewer per subject), `--headless` / `--defer-review` (stop at the gate, review later) and `--no-checkreg`. Approvals still go to the method log only.

### 12.5 Wyss DBS Localizer implementation checklist

Status of the checklist: all items except re-compute are implemented in the *Registration review* tab (§12.4a); re-compute is not implemented (the *Recompute* button was removed, `08` addendum).

- [ ] **State machine** for ordered review list + `activevolume` + last-slide Close labels.  
- [ ] **Approval** JSON structure as in §12.4a (method log approvals for coreg and norm).  
- [ ] **Re-compute** either stubbed with **UNSPECIFIED** pipeline IDs or mapped to `dbsloc` stages when they exist.  
- [ ] **Regenerate figure** = same contract as §12.2.  
- [ ] **Open viewer** = separate window with pan/zoom/scroll (reimplement or embed webGPU surface).

---

*End spec 03.*
