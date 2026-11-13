# Spec 02 — Preprocessing (Wyss DBS Localizer)

**Status:** Phase B; §4.3, §6.1 and §7.1 are normative and describe the implemented behaviour; §4.1 (except its N4 step, see §4.3), §4.2, §5 and §8 are original design text that is not implemented  
**Depends on:** `01_bids.md`, `00_overview.md`

---

## 1. Purpose

Define **anatomical preprocessing** from BIDS **raw** NIfTIs to **preproc** volumes (reorient, bias correction, optional anchor resampling) and related **side products** (PaCER mask path, optional precoreg — precoreg detailed in **§6** and cross-`03`).

**Out of scope:** DICOM conversion (import spec), post-op block that only copies raw→preproc without N4 (see §5), full **AC-PC** pipeline (`00` / dedicated tool spec).

---

## 2. Inputs

| Input | Type | Notes |
|-------|------|------|
| `Subject.preop_anat[mod].raw` | path | Must exist when stage runs |
| `Subject.preop_anat[mod].preproc` | path | Target path to create |
| `prefs.nifti_ext` | enum | `.nii` vs `.nii.gz` — copy/gunzip behaviour from raw |
| `Subject` with `postop_anat` present or not | bool | Controls anchor reslice (§4) |
| `template_space` / template NIfTI stem for precoreg | from `01` | For `precoreg` only |
| External: N4 binary, resample binary, optional dcm2nii-class reorient | paths in `Prefs` | Subprocess |

**Units:** millimetres in NIfTI world space; voxel sizes in mm (header `pixdim`).

---

## 3. Outputs

| Output | Condition |
|--------|-----------|
| Preproc NIfTI per pre-op modality | Always when raw exists and preproc missing |
| Anchor resliced to **0.7 mm** isotropic | Anchor only, if **post-op** present and any voxel dim **> 0.7 mm** |
| Precoreg **coreg** NIfTI + **.mat** `tmat` | If `coreg` output missing (or policy says recompute) — **§6** |
| `recon.raw_ct_mask` / `anchor_native_mask` | From `gen_ct_mask` when PaCER path runs — **§7** |

---

## 4. Algorithm — pre-op branch

**Order** (per `00_overview` orchestration):

```
for each modality in preop_anat (BIDS order: anchor first in struct = first in list):
  if preproc file missing:
    copy raw → preproc (respect .gz)
    anat_preprocess(preproc_path)
  if modality is anchor AND postop dataset present:
    reslice_anchor_if_coarse(preproc_path)
// then precoreg when coreg slot empty (§6)
```

### 4.1 `anat_preprocess(path)`

1. **Reorient + crop** (`rocrop`): produce canonical orientation and FOV; reject or retry if output **&lt; 10 KB** (corrupt); max **10** retries; on total failure **warn** and leave file unchanged.
2. **Bias field** (`n4_bias_correct`): default **N4**-class correction, 3D, in-place:
   - shrink 4, B-spline `[200]`, convergence `[50x50x50x50,ε]` (ε = 1e-6 class).
   - **Implementation note:** Wyss DBS Localizer **should** write to temp file then replace to avoid partial corrupt in-place.

**Intent:** RAS/LPS convention and brain FOV — exact tool **configurable** (dcm2nii-class **not** mandatory if equivalent result).

### 4.2 `reslice_anchor_if_coarse(path)`

- Read voxel spacing from NIfTI header.
- If **any** spacing **&gt; 0.7 mm**: resample to **0.7×0.7×0.7 mm** isotropic, **linear** interpolation, **pad** +5 voxels per axis (algorithm class: `ResampleImageBySpacing`), **in-place** on path.
- If all dimensions **≤ 0.7 mm**: **no** upsampling (do not force 0.6 → 0.7).

**Skip** entire `reslice_anchor_if_coarse` if dataset is **pre-op only** (no post-op anatomy) — saves compute.

### 4.3 Wyss DBS Localizer `preprocess` stage (normative for Wyss DBS Localizer)

- Runs `N4BiasFieldCorrection -d 3 -i <raw anchor> -o <short work name>` committed as `preprocessing/anat/<raw entities>_desc-preproc_<mod>.nii.gz` (`01` §7a) (ANTs CLI defaults: mask of ones, shrink factor 4, `[50x50x50x50,0.0]`, `[1x1x1,3]`; argv from `crates/dbsloc-pipeline/src/ants_argv.rs` `build_n4_argv`). Only the anchor is corrected; reorient/crop and the 0.7 mm reslice of §4.1–4.2 are not implemented. Measured wall time: 37–54 s per subject at 4 ITK threads (Lead-Tutor cohort logs, 2026-10-07), 33 s on `57245` in isolation (2026-10-08; `docs/gpu_n4_report.md`).
- **Execution backend** (`--reg-backend auto|wgpu|ants`, or `--preprocess-backend` for this step and `precoreg` alone, the `specs/04` §5.4 rule): `ants` runs the `N4BiasFieldCorrection` process above; `wgpu` runs the in-process N4 (`fastwarp_gpu::GpuN4`, or the `fastwarp_reg::n4` CPU `f64` reference); `auto` picks the GPU when a hardware adapter passes the device self-test and otherwise ANTs. The in-process body runs on a dedicated rayon pool of `ants_max_threads` workers when that option is set (`specs/04` §5.4 step 3, `reg_backend::with_worker_threads`). The in-process port reproduces the ANTs CLI defaults exactly (all-ones mask, shrink 4, `[50x50x50x50,0.0]`, `[1x1x1,3]`), evaluates the bias field at full resolution and writes the same float32 gzip output. Deviations: the GPU kernels are `f32`, and the scattered fit gathers each control point's fixed voxel box instead of ITK's per-thread scatter, so the sum has one fixed order (the CPU reference uses the same order; byte identity with ANTs is not a goal). On the in-process path the stage writes the sidecar `preprocessing/anat/<raw entities>_desc-preproc_<mod>.json` with `reg_backend`, `reg_requested`, `reg_note`, `reg_adapter`, `reg_timings` (`read_s`, `n4_s`, `write_s`, `total_s`), the N4 parameters, and the per-level convergence values (logged, never used to stop, since the threshold is 0). The `preprocess` lock follows the backend rule: `auto` accepts either backend's approved outputs, an explicit request re-runs when the recorded backend differs, and no record counts as `ants`; prerequisites skip the `N4BiasFieldCorrection` check when the backend resolves to the GPU, and `auto` falls back to ANTs on any GPU error before the output is staged. **Measured on the Lead-Tutor cohort** (Arc 140V, Vulkan, `--reg-backend wgpu`, 2026-10-08): the in-process N4 takes 9.7–23.2 s (`n4_s`) and 10.5–24.1 s including read and write (`total_s`), against the bundled ANTs 37–54 s — about twice as fast, but above the plan's ≤ 3 s target and still host-overhead-bound. The cohort marker error is **1.076 mm** over 20 leads, against 1.092 mm for ANTs (Δ −0.017; D9 passes; largest per-lead change 0.643 mm, `15454` +x; `docs/gpu_n4_report.md`). The `f64` CPU reference matches the bundled ANTs to a mean |ratio − 1| ≈ 1e-6 inside the head, and the GPU matches the CPU reference to ≈ 3e-5 relative per voxel (small case; the parity tests assert <= 1e-4 on the reduced schedule and <= 2e-4 at the production 4x50 schedule, where the measured relative error is 1.36e-4, `fastwarp/crates/fastwarp-gpu/tests/n4_parity.rs`); a second run of two subjects is byte-identical in `Composite.h5`, `InverseComposite.h5` and `_Warped.nii.gz`.
- **Anchor selection downstream:** coregistration (fixed image) and normalization (moving image) use the preprocessed anchor **whenever that file exists on disk** (and the precoreg image before it, §6.1) at the time the stage starts, and the raw anchor otherwise. It does not matter whether `preprocess` ran in the same invocation; the subject is re-resolved before every stage. (The removed Python pipeline used the N4 file only when `preprocess` ran in the same run, so a later `--stages coreg` registered against the uncorrected raw image.)
- Result token `ok:preprocess:n4:<file name>`; when the backend resolves to ANTs, a missing N4 binary is the prerequisite error `preprocess: N4BiasFieldCorrection not found (bundled or PATH)`.
- `precoreg` (§6) runs as specified in §6.1 (rigid, header-only); once its image exists it is the anchor in use for every later stage.

---

## 5. Post-op preproc (minimal)

Not implemented: the `preprocess` stage corrects only the anchor (§4.3) and does not copy post-op images to `preprocessing/`; the policy flag `preprocess_postop_same_as_preop` does not exist. Design text: when only **copy** raw→preproc is required (no N4 for that block), **gunzip/copy** only.

---

## 6. Precoregistration (precoreg)

**Goal:** Roughly align **anchor preproc** to **template** so later registration has a sane start; **no reslice** of volume data — **affine in header** update on a **copy**.

**Inputs:** `anchor_preproc_path`, `template_stem` (e.g. `t1`), `out_coreg_path`, `out_mat_path`.

**Steps:**

1. If `out_coreg_path` absent: **copy** `anchor_preproc_path` → `out_coreg_path`.
2. Run **9-dof mutual-information** linear registration **fixed** = template NIfTI, **moving** = `out_coreg_path` (implementation class: SPM `coreg` / ANTs—**locked in Prefs**).
3. Save **4×4** world matrix `tmat` to `out_mat_path`.
4. Update **only** `out_coreg_path` NIfTI affines so the image grid matches the solved alignment **without** resampling voxels (reheader).

**Fallback:** If output NIfTI **fails validation** (load): delete `out_mat_path`, **copy** `anchor_preproc_path` → `out_coreg_path` (identity behaviour).

**Lock:** gated with `reg_locked(out_coreg_path)` per `00`.

### 6.1 Wyss DBS Localizer `precoreg` stage (normative for Wyss DBS Localizer)

- **Degrees of freedom: rigid (6), not 9.** The result only rewrites the header, so any scale in it would change the subject's millimetres in anchor-native space, where the electrode model is fitted in physical mm (`05` §4.4). Deviation from the reference's 9-dof recipe, deliberate.
- **Inputs:** moving = the anchor of §4.3 (N4 output when present, else raw); fixed = the resolved template T1w (`04` §3.1).
- **Engine** follows `RunOptions.backend_for(Stage::Precoreg)` (the preprocess override, else `reg_backend`) with the rule of `04` §5.4 (`wgpu` in-process rigid MI from `fastwarp-reg` / `fastwarp-gpu`; `ants` = `antsRegistration` with the argv below; `auto` = `wgpu` when an adapter passes the self-test, else `ants`). ANTs argv: `--dimensionality 3 --output [<pfx>] --initial-moving-transform [fixed,moving,1] --transform Rigid[0.1] --metric MI[fixed,moving,1,32,Regular,0.25] --convergence [1000x500x250,1e-6,10] --shrink-factors 4x2x1 --smoothing-sigmas 2x1x0vox --random-seed 20260910`, then the `--ants-…` pass-through tokens. No `--output` warped image: nothing is resliced.
- **Outputs** (staged through `.partial-*`, `00` §5.2): `coregistration/transformations/sub-<id>_ses-preop_from-<mod>_to-<Space>_desc-precoreg_mode-image_xfm.mat` (the ITK transform as written by the engine) and `coregistration/anat/<raw anchor entities>_desc-precoreg_<mod>.nii.gz` (`01` §7a): the moving image's voxels unchanged, both qform and sform replaced by `A' = M · A`, where `A` is the moving image's voxel → world (RAS mm) affine and `M` is the 4×4 RAS world map moving → fixed (the inverse of the ITK fixed → moving point map, converted LPS → RAS). `sform_code` / `qform_code` keep their values (or 1 / scanner when they were 0). Header record `coregistration/log/sub-<id>_desc-precoreg.json`: `{backend, template, template_space, matrix_ras (4×4 row-major), rotation_deg, rotation_angle_deg, translation_mm, moving, reg_backend, reg_requested, reg_note, reg_adapter, reg_timings?}`; `rotation_deg` = Euler angles `[rx, ry, rz]` in degrees with `R = Rz·Ry·Rx` (RAS), `rotation_angle_deg` = the axis-angle magnitude.
- **Stale precoreg.** A successful `preprocess` makes a new N4 anchor, so the precoreg image and its record (built from the old one) are part of `preprocess`'s staged set and are **removed** when it commits; the next `precoreg` rebuilds them. A failed `preprocess` keeps them.
- **Lock.** An approved `precoreg` follows the backend rule of `04` §5.4: `auto` accepts either backend; an explicit request re-runs when the record's `reg_backend` differs; a missing record accepts the approved outputs; a record without the key counts as `ants`.
- **Downstream use:** once the precoreg image exists it **is the anchor** for `coreg` (fixed image) and `normalize` (moving image), and for everything that reads the anchor afterwards (checkreg, scene viewer). Anchor-native space is therefore the precoreg'd frame; template-space results are unchanged in principle (the rigid map is absorbed by the normalization). Without the file the stages read the §4.3 anchor as before.
- **Fallback:** when the written NIfTI fails to load, discard the staged outputs (the previous outputs and lock stay, `00` §5.2) and fail the stage with `precoreg: written image does not load: …` (stricter than the reference's silent identity copy).
- **Post-condition / acceptance:** sampling the precoreg'd image at template world coordinates equals `antsApplyTransforms` of the moving image onto the template grid with the same transform (linear interpolation; RMS difference < 1 % of the image's robust range on the synthetic head); voxel data byte-identical to the input; the rotation part of `A'·A⁻¹` orthonormal to 1e-6.
- **Result token:** `ok:precoreg:<precoreg image stem>`, e.g. `ok:precoreg:sub-01_ses-preop_desc-precoreg_T1w`.

---

## 7. CT brain mask for PaCER (`gen_ct_mask`)

**When:** Reconstruction PaCER path (not main autocoord preprocessing); listed here as **derived image** spec.

**Steps (logical):**

1. Place template **brain mask** in subject space via **normalization** apply (inverse or per `01` paths).
2. Apply **CT coregistration** transform (from `coreg.log.method` engine: ANTs / BRAINSFit / FLIRT) to bring mask to **anchor native** grid for PaCER input.
3. Output paths: `raw_ct_mask`, `anchor_native_mask` per `01_bids`.

### 7.1 Wyss DBS Localizer `gen_ct_mask` (normative, 2026-10-12)

- **Template mask.** The shipped template gains `resources/templates/space/MNI152NLin2009bAsym/res-1mm/brainmask.nii.gz` (uint8 0/1 on the shipped T1w grid). Built by `scripts/build_shipped_template.py --mask-from <mni_icbm152_nlin_asym_09c_nifti.zip>` (zip pinned by the catalogue SHA-256): member `mni_icbm152_nlin_asym_09c/mni_icbm152_t1_tal_nlin_asym_09c_mask.nii`, carried onto the 2009bAsym 1.0 mm grid through the template space bridge (`04` §5.4a, 2009cAsym → 2009bAsym), linear interpolation, threshold 0.5. Licence: same MNI `COPYING` as the T1w / T2w. A template directory may hold its own `brainmask.nii.gz`; otherwise the reference-space mask is bridged into the template space exactly as atlases are (`08` §5).
- **When.** In `reconstruct`, before PaCER runs (`recon_method = pacer`, and the PaCER cross-check), only for post-op CT, and only when normalization and CT coregistration outputs exist; otherwise PaCER keeps its own mask (`10` §4) and the stage logs `dbsloc: note: no template brain mask; PaCER computes its own`.
- **Steps.** (1) Template mask → anchor-native on the anchor grid with the inverse normalization (`from-<Space>_to-anchorNative` composite; backend per `04` §5.4, in-process apply for `wgpu`), linear, threshold 0.5 → `anchor_native_mask`. (2) Anchor-native mask → raw CT grid with the inverse of the CT coregistration (`antsApplyTransforms -r <raw CT> -t [<from-CT_to-anchorNative .mat>,1]`, or the in-process equivalent), linear, threshold 0.5 → `raw_ct_mask`. Both written through the staged write, names per `01` §7a. The `antsApplyTransforms` calls of both steps receive **no** `--ants-…` pass-through tokens (the argv is fixed). `raw_ct_mask` is always written, although the pipeline's PaCER uses `anchor_native_mask` (below).
- **Use.** PaCER (`10`) receives as `brain_mask` the first of `raw_ct_mask`, `anchor_native_mask` whose grid equals the CT it segments (the pipeline's PaCER segments the coregistered CT on the anchor grid, so in practice `anchor_native_mask`; a candidate on another grid is skipped, and when none matches the stage logs `dbsloc: note: reconstruct: no template brain mask lies on the CT grid PaCER segments; PaCER computes its own` and PaCER computes its own mask, `ct_recon.rs`). A provided mask is eroded exactly like the computed one (`10` §4, ~3 mm). Reco `meta.pacer_mask` = `"template"` or `"own"`. `recon_method = auto` (TRAC/CORE) does not run PaCER and does not build the masks. Shipping the template mask also activates the reconstruction in-brain plausibility gate (`05`) on the default template.
- **Acceptance.** On the synthetic head, the CT mask covers ≥ 95 % of the synthetic brain and < 5 % of the skull voxels; on the cohort, PaCER results with the template mask are reported next to its own mask (no acceptance threshold: PaCER is a cross-check).

---

## 8. Legacy: multi-contrast composite

**Optional** `gen_contrast_image(volume4d, mode)`: combines echo/plane channels (product, mean, smooth) — **no** static call graph in reference tree; **defer** to `05` / research if needed.

---

## 9. Edge cases and errors

| Condition | Behaviour |
|-----------|-----------|
| Raw missing | `no anchor raw` or `anchor raw file missing` (stage fails) |
| N4 failure | `N4 failed with <code>` (ANTs process) or `preprocess: N4 on <engine> failed: …` (in-process engine); the half-written file is not committed (staged write, `00` §5.2) |
| Reslice OOM | not applicable: the reslice is not implemented (§4.3) |
| Precoreg template missing | `precoreg: no normalization template …` |

The `E_RAW_MISSING`, `E_N4`, `E_RESAMPLE` and `E_TEMPLATE` codes of the original design are not emitted.

---

## 10. Acceptance criteria

- [ ] Golden raw NIfTI → preproc passes **header** sanity (dims, no NaN slice).
- Not implemented (§4.3): the file &lt; 10 KB retry path, the 0.7 mm anchor reslice (1.0 mm anchor + post-op → 0.7 mm isotropic) and the pre-op-only no-reslice flag.
- [ ] Precoreg: round-trip **position** of template center of mass within tolerance (config mm) — smoke test.

---

## 11. References

- Tustison et al. N4 (2010).  
- ANTs / SPM coregistration citations in `03` / `04`.

---

*End spec 02.*
