# Spec 01 — BIDS Ingestion & Subject Data Layout (Wyss DBS Localizer)

**Status:** Phase B (language-agnostic)  
**Depends on:** `00_overview.md`  
**Phase A source:** the Phase A archive (removed 2026-10-23, in git history) — section `specs/phase_a_reports/01_bids_ingestion.md` (path inventory; this doc is the **contract**).

---

## 1. Purpose and scope

Specify **dataset directory layout** for a Lead-DBS–class BIDS superset, **subject identity rules**, the `**Subject` record** (all paths a runner needs), and **naming** for NIfTIs, transforms, and logs. **DICOM** import: requirements and outputs only — not vendor-specific SOPs.

**Out of scope here:** bit-for-bit compatibility with every legacy pre-BIDS folder; migration tools may be a separate spec.

---

## 2. Dataset root

```
<dataset_root>/
  dataset_description.json
  .bidsignore
  rawdata/
    sub-<id>/
  sourcedata/
    sub-<id>/
  derivatives/
    leaddbs/
      sub-<id>/
    freesurfer/          (optional, shared)
```

- `**dataset_description.json`:** BIDS mandatory; `PipelineDescription` or equivalent may name Wyss DBS Localizer.
- `**.bidsignore`:** may list patterns for Lead-DBS–specific derivatives to silence generic BIDS validation.

**Units:** NIfTI **mm** in world coordinates, **LPS** or **RAS** per NIfTI header; Wyss DBS Localizer normalizes to one convention in I/O (see `02` / shared I/O spec).

**Voxel → world affine (normative, every reader in Python and Rust):** use the **sform** (`srow_x/y/z`) when `sform_code > 0`. Otherwise use the **qform** when `qform_code > 0`: rotation from the quaternion (`b, c, d` = `quatern_b/c/d`, `a = sqrt(max(0, 1 − b² − c² − d²))`), columns scaled by `pixdim[1..3]`, the third column also multiplied by `qfac = pixdim[0]` (taken as `1` unless it is `−1`), and translation `qoffset_x/y/z` (NIfTI-1 standard, method 2). Only when both codes are `0`, fall back to `diag(pixdim[1..3])` with zero origin (method 1). This matches nibabel's `get_best_affine`. Ignoring the qform misplaces any qform-only image by its full origin and rotation, which is wrong for centroids, coregistration starts (`03` §5.1) and every world-coordinate sample.

---

## 3. Subject ID

- Pattern: `sub-<id>` on disk; `**<id>`** = one token matching `^[^\W_]+$` (alphanumeric, no internal underscore) — for portability with legacy `sub-` rules.
- **Uniqueness:** One BIDS `sub-` per patient in a dataset; duplicate IDs = **error** at `getSubj` time.
- **Listing subjects:** Union of `sub-*` under `sourcedata/`, `rawdata/`, and `derivatives/leaddbs/` (unique by id).

---

## 4. Sessions and modalities

- **Session folders:** `ses-preop` and `ses-postop` under `rawdata/sub-<id>/` (lowercase, fixed literals for the modern layout).
- **Anatomy rawdata** lives under `ses-<name>/anat/`. Optional: `func/`, `dwi/`, `ses-*/unsorted/` as staging.
- **Post-op modality (logical):** from raw images and `rawimages.json` index: `CT`, or `MRI` (optionally with `acq-ax` / `cor` / `sag`), or `None` if post-op not used.

**Anchor modality (logical):** first **pre-op** anat in **priority** order: `T1w`, `T2w`, `PDw`, `T2starw` (or extended list in **prefs**), then remaining modalities sorted per **spacedef** + alphabetical tie-break. Stored as a **BIDS filename suffix token** (e.g. `T1w`, `T2w`) with optional `acq-` and `rec-` entities — see §8.

---

## 5. Subject derivative tree (`subj_dir`)

`subj_dir` = `derivatives/leaddbs/sub-<id>`


| Subdirectory                                                                                                 | Purpose                                                     |
| ------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------- |
| `prefs/`                                                                                                     | Per-subject preference overrides (JSON/legacy MAT)          |
| `log/`                                                                                                       | Subject-level run logs                                      |
| `preprocessing/anat/`                                                                                        | Bias-corrected, reoriented copies                           |
| `coregistration/anat/`, `coregistration/log/`, `coregistration/checkreg/`, `coregistration/transformations/` | Coreg outputs + QC + affines                                |
| `normalization/anat/`, `normalization/log/`, `normalization/checkreg/`, `normalization/transformations/`     | Norm outputs + warps/affines                                |
| `brainshift/…`                                                                                               | Brain-shift workflow (anat, log, checkreg, transformations) |
| `reconstruction/`                                                                                            | `*_desc-reconstruction.mat`, masks                          |
| `stimulations/`                                                                                              | VAT/VTA NIfTIs (native/ template subfolders)                |
| `acpc/`, `atlases/`, `export/`, `clinical/`, `warpdrive/`                                                    | as needed                                                   |


`freesurferDir` may be `derivatives/freesurfer` **shared** across subjects.

---

## 6. The `Subject` record (path contract)

**Type name:** `Subject` (see `00_overview` §4.1). All **path** fields are **absolute** when resolved. Optional fields may be **empty** if stage not run.

### 6.1 Core


| Field               | Type   | Description                                   |
| ------------------- | ------ | --------------------------------------------- |
| `subj_id`           | string | ID without `sub-` prefix                      |
| `subj_dir`          | path   | `…/derivatives/leaddbs/sub-<id>`              |
| `rawdata_dir`       | path   | `…/rawdata/sub-<id>`                          |
| `sourcedata_dir`    | path   | `…/sourcedata/sub-<id>`                       |
| `anchor_modality`   | string | Key for pre-op (e.g. `iso_T1w`, `T1w`) per §4 |
| `postop_modality`   | enum   | `MRI` | `CT` | `None`                         |
| `both_mrct_present` | bool   | True if post-op has both CT and MR raw        |


**Directory field bundle** (sibling paths under `subj_dir`): `atlas_dir`, `brainshift_dir`, `coreg_dir`, `export_dir`, `log_dir`, `norm_dir`, `prefs_dir`, `preproc_dir`, `recon_dir`, `stim_dir`, `warpdrive_dir`, `acpc_dir`, `clinical_dir`, `freesurfer_dir` (may be above `subj_dir`).

**Misc paths:** `uiprefs` (per-subject mat/json), `method_log` (methods text for publications).

### 6.2 Pre-op anatomy `preop_anat[modality]`

**Per modality** (modality = struct field name matching BIDS file):


| Key       | When set                                                           |
| --------- | ------------------------------------------------------------------ |
| `raw`     | rawdata anat NIfTI                                                 |
| `preproc` | preprocessed in `preprocessing/anat`                               |
| `coreg`   | in `coregistration/anat`, `space-anchorNative`                     |
| `norm`    | **anchor only** — in `normalization/anat`, `space-<TemplateSpace>` |


### 6.3 Post-op `postop_anat[modality]`

Keys include `CT` and/or `ax_MRI` / `cor_MRI` / `sag_MRI`. **Per key:**


| Sub-key                                             | Content                                                          |
| --------------------------------------------------- | ---------------------------------------------------------------- |
| `raw` / `preproc` / `coreg` / `norm`                | Same pattern as pre-op, `ses-postop` in filename                 |
| `coreg_tonemap` / `norm_tonemap` / `*Scrf` variants | CT + brainshift variants; see Phase A for exact BIDS token names |


### 6.4 Coregistration bundle `coreg`

- `**coreg.anat.preop[mod]`**, `**coreg.anat.postop[...]**` — resolved paths.  
- `**coreg.transform[mod]**` (anchor): precoreg `.mat` path.  
- `**coreg.transform[mod].forward_base**`, `**inverse_base**`: base name **without** engine suffix, for `from-<A>_to-<B>_desc-` BIDS style; `CT` has dedicated forward/inverse to anchor.  
- `**coreg.log.method`**, `**coreg.log.log_base**` — JSON and diary prefix.  
- `**coreg.checkreg[session][mod]**` — optional QC PNG path map.

### 6.5 Normalization `norm`

- `**norm.anat.preop[anchor_mod]**` — template-space anchor.  
- `**norm.anat.postop[...]**` — post-op in template space.  
- `**norm.transform.forward_base**`, `**inverse_base**` — must join with suffix `ants.nii.gz` or `ants.mat` or `fnirt.nii.gz` per method (see `04_normalization.md`).  
- `**norm.log.method**`, `**norm.log.log_base**`.  
- `**norm.checkreg**`.

### 6.6 Brain shift `brainshift`

- `brainshift.anat.moving`, `anchor`, `scrf` — nifti paths.  
- `secondstep_mask`, `thirdstep_mask` — for masked affine (DiODe/SCRF workflow).  
- `transform.instore` / `converted` / `scrf` — three `.mat` roles in approval workflow.  
- `brainshift.log.method`, checkreg paths.

### 6.7 Reconstruction `recon`


| Field                                | Use                                                  |
| ------------------------------------ | ---------------------------------------------------- |
| `recon`                              | `*_desc-reconstruction.mat` (contacts, trajectories) |
| `raw_ct_mask` / `anchor_native_mask` | PaCER brain mask paths (CT pipeline)                 |


### 6.8 Other

- `**stats**`, `**acpc**`, `**stim_dir**` (base for VAT) — per Phase A / module specs.

---

## 7. BIDS file naming (derivatives, representative)

- **Base pattern:** `sub-<id>_ses-<preop\|postop>[_acq-<label>][_rec-<label>]_space-<Space>_desc-<Desc>_acq-<Contrast>_<suffix>.nii[.gz]`
- **Spaces:** `anchorNative`, `<TemplateSpaceName>` (from `spacedef.name`, e.g. `MNI152NLin2009bAsym`), `rawCT` for masks, etc.  
- **Desc tokens:** `preproc`, `precoreg`, `normmethod`, `coregmethod`, `brainshift`, `secondstepmask`, `tonemapped`, etc. (exact set = compatibility table in migration doc if needed).  
- **Transform basenames** end with `_desc-` and concatenate engine-specific extension in code — **Wyss DBS Localizer** should treat **(base, suffix)** as a pair in API.

### 7a. Wyss DBS Localizer derivative names (normative, 2026-10-12)

**Output root (2026-10-22).** The derivative tree `sub-<id>/…` lives under `<dataset>/derivatives/leaddbs` by default; `RunOptions.output_dir` (`--output-dir`, `DBSLOC_OUTPUT_DIR`, the GUI's *Output folder* row) moves the whole tree, locks and the Lead-DBS prefs index included, to another folder. Raw images are always read from `<dataset>/rawdata`. One function derives the root (`paths::derivatives_root`); every front end and the import go through it.

One module owns every derivative path: `crates/dbsloc-pipeline/src/derivatives.rs`. No other code formats a derivative file name. Older names are **not** read (no compatibility: pre-production); a tree made by an older version is re-run.

**Image rule.** A derivative image of a raw image keeps the raw file's entities in order and inserts `space-<S>` and `desc-<D>` before the suffix: raw `sub-01_ses-postop_acq-ax_T2w.nii.gz` → `sub-01_ses-postop_acq-ax_space-anchorNative_desc-preproc_T2w.nii.gz`. An entity already present is replaced, not duplicated. `<Space>` is the template's space id (`04` §3.1, e.g. `MNI152NLin2009bAsym`); no `res-` entity (the method log records the template). The raw extension is not kept: every derivative image is `.nii.gz`.

**Transform rule** (fMRIPrep style): `sub-<id>[_ses-<s>][_acq-<a>]_from-<A>_to-<B>[_desc-<D>]_mode-image_xfm.<ext>`; `<A>` / `<B>` are a modality suffix (`T1w`, `T2w`, `CT`, `MRI`), `anchorNative` or `<Space>`; `<ext>` = `mat` (ITK affine as written by ANTs or the in-process engine) or `h5` (ITK composite). ANTs is run with a work prefix inside the stage's `.partial-*` staging and its fixed output suffixes (`0GenericAffine.mat`, `Composite.h5`, `InverseComposite.h5`, `_Warped.nii.gz`) are renamed to these names when the stage commits (`00` §5.2).

| Stage | Output | Name (under `derivatives/leaddbs/sub-<id>/`) |
| --- | --- | --- |
| preprocess | N4 anchor | `preprocessing/anat/<raw entities>_desc-preproc_<mod>.nii.gz` + sidecar `….json` (former `_N4.json` content) |
| precoreg | reheadered anchor | `coregistration/anat/<raw entities>_desc-precoreg_<mod>.nii.gz` |
| precoreg | transform | `coregistration/transformations/sub-<id>_ses-preop_from-<mod>_to-<Space>_desc-precoreg_mode-image_xfm.mat` |
| precoreg | record | `coregistration/log/sub-<id>_desc-precoreg.json` |
| coreg | CT in anchor space | `coregistration/anat/<raw CT entities>_space-anchorNative_desc-preproc_CT.nii.gz` |
| coreg | CT transforms | `coregistration/transformations/sub-<id>_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat`; composites `…_from-CT_to-anchorNative_mode-image_xfm.h5` and `…_from-anchorNative_to-CT_mode-image_xfm.h5` when written |
| coreg | multistart record + work files | `coregistration/log/sub-<id>_ses-postop_desc-multistart_CT.json`; work files in `coregistration/log/multistart/` |
| coreg | post-op MR in anchor space | `coregistration/anat/<raw MR entities>_space-anchorNative_desc-preproc_<suffix>.nii.gz` (`<suffix>` = the raw file's suffix) |
| coreg | post-op MR transform | `coregistration/transformations/sub-<id>_ses-postop[_acq-<a>]_from-<suffix>_to-anchorNative_mode-image_xfm.mat` (`<a>` = the raw file's `acq`, else the orientation of the bundle key: `ax` / `cor` / `sag` for `ax_MRI` / `cor_MRI` / `sag_MRI`) |
| normalize | T2w in anchor space | `coregistration/anat/<raw T2w entities>_space-anchorNative_desc-preproc_T2w.nii.gz` + `coregistration/transformations/sub-<id>_ses-preop[_acq-<a>]_from-T2w_to-anchorNative_mode-image_xfm.mat` |
| normalize | anchor in template space | `normalization/anat/<raw anchor entities>_space-<Space>_desc-preproc_<mod>.nii.gz` |
| normalize | post-op images in template space | `normalization/anat/<raw entities>_space-<Space>_desc-preproc_<suffix>.nii.gz` (CT and every MR key) |
| normalize | warps | `normalization/transformations/sub-<id>_from-anchorNative_to-<Space>_mode-image_xfm.h5` (forward composite) and `sub-<id>_from-<Space>_to-anchorNative_mode-image_xfm.h5` (inverse) |
| normalize | linear part (2026-10-22) | `normalization/transformations/sub-<id>_from-anchorNative_to-<Space>_desc-affine_mode-image_xfm.mat` (ITK binary affine = the rigid+affine element of the forward composite; ITK convention: maps template points to native points) and `sub-<id>_from-<Space>_to-anchorNative_desc-affine_mode-image_xfm.mat` (its inverse); written by both engines in the same staged set as the composites; an approved lock without them gets them back-filled from the approved forward composite on the next run (note logged, method log untouched). The method log lists `transforms: {forward, inverse, forward_affine, inverse_affine}` and `affine_native_to_template_4x4` (RAS mm, row-major, native → template) |
| normalize | registration grid scratch | `normalization/work/`; removed at stage end (no `_resampled_is*mm/` left behind); the normalize checkreg gate writes `template_is<mm>mm.nii.gz` there again and removes the folder once the review approves (`04` §5.4 step 3) |
| coreg / normalize / brainshift | method logs (`03` §3, `04` §14, QC gate `03` §12.4a) | `coregistration/log/sub-<id>_desc-coregmethod.json`, `normalization/log/sub-<id>_desc-normmethod.json`, `brainshift/log/sub-<id>_desc-brainshiftmethod.json` |
| all registration stages | checkreg PNG | `<stage dir>/checkreg/<image stem>.png` (`<image stem>` = the checked image's name without `.nii.gz`) |
| brainshift | outputs | reserved names `brainshift/anat/<raw CT entities>_space-anchorNative_desc-brainshift_CT.nii.gz`, `brainshift/transformations/sub-<id>_ses-postop_from-anchorNative_to-anchorNative_desc-brainshift_mode-image_xfm.mat`; no helper in `derivatives.rs` produces them yet: the only brainshift body (`DBSLOC_SCRF_ANTS=1`, `00` §5.2) takes its input and output paths from `DBSLOC_SCRF_MOVING` / `_REFERENCE` / `_OUTPUT` / `_WARP` / `_AFFINE`; only the method log below is named here |
| reconstruct | Reco v2 | `reconstruction/sub-<id>_desc-reconstruction.json` (unchanged) |
| reconstruct | Lead-DBS compatible twin (2026-10-22) | `reconstruction/sub-<id>_desc-reconstruction.mat` (MATLAB v7 Level-5, variables `reco` + `dbsloc_provenance`, `specs/12` Part 1), rewritten whenever the JSON is written (reconstruct, orientation, manual correction); the JSON stays the primary file |
| reconstruct | PaCER brain masks (`02` §7) | `reconstruction/<raw CT entities>_space-CT_desc-brain_mask.nii.gz`, `reconstruction/<raw CT entities>_space-anchorNative_desc-brain_mask.nii.gz` |
| visualize | scene manifest | `reconstruction/viz/scene_manifest.json` (unchanged) |
| vat (2026-10-23, pending) | canonical stimulation settings | `stimulations/native/<label>/sub-<id>_desc-stimulation.json` (`07` §2.3) |
| vat (pending) | Lead-DBS settings twin | `stimulations/<space>/<label>/sub-<id>_desc-stimparameters.mat` (MAT v7) in every space folder of the label; `<space>` = `native`, `<Space>`, and `MNI152NLin2009bAsym` when it differs from `<Space>` (`07` §4 step 3, §5.1) |
| vat (pending) | VTA images (uncompressed `.nii`, see below) | `stimulations/<space>/<label>/sub-<id>_sim-binary_model-<slug>_hemi-<H>.nii`; for model `efield` also `…_sim-efield_model-<slug>_hemi-<H>.nii` and `…_sim-efieldgauss_model-<slug>_hemi-<H>.nii` (`<slug>` = the Lead-DBS file slug of `07` §1.1, `simbio` for `efield`; `<H>` = `R` or `L`) |
| vat (pending) | VTA mesh and volume | `stimulations/<space>/<label>/sub-<id>_sim-binary_model-<slug>_hemi-<H>.mat` (MAT v7, `07` §5.2) |
| vat (pending) | results sidecar, atlas statistics | `stimulations/<space>/<label>/sub-<id>_desc-vta.json` (every space folder); `sub-<id>_desc-vtastats.json` (template and reference-space folders, `07` §8.4) |
| vat / group (pending) | Lead-DBS statistics | `sub-<id>_desc-stats.mat` at the subject folder root (`ea_stats`, MAT v7; `07` §8.4, `14` §3); backup `sub-<id>_desc-stats_backup.mat` (`14` §3.4) |
| vat (pending) | E-field basis cache | `headmodel/native/sub-<id>_hemi-<H>_desc-efieldbasis.<ext>` + `….json` (internal format, `07` §3.6; Lead-DBS's own files in that folder are left alone) |
| group (pending) | mirrored stimulation images | `stimulations/<Space>/gs_<guid>/…_hemi-R_hemidesc-FlippedFromLeft.nii` and `…_hemi-L_hemidesc-FlippedFromRight.nii` beside the originals (`14` §4.4) |
| connectivity job (pending) | seeds, maps, per-fibre values | in `stimulations/<Space>/<label>/` (`15` §9.2): `sub-<id>_sim-<kind>_model-<slug>_seed-dMRI.nii` and `…_seed-fMRI.nii` (each + `_hemi-L` / `_hemi-R`), `…_seed-dMRI_conn-<tag>_strucmap.nii`, `…_seed-fMRI_conn-<label>_desc-{AvgR,VarR,AvgRFz,T}_funcmap.nii`, optional `…_seed-fMRI_conn-<label>_id-<subject id>_corr.nii`, `sub-<id>_sim-efield_model-<slug>_hemi-<H>_conn-<tag>_fibervals.mat`, `…_seed-dMRI_conn-<tag>_fibers.mat`; each with a provenance sidecar `<name>.json` (`15` §9.3) |

**Stimulation, group and connectome derivatives (amendment 2026-10-23, Phase B; implementation pending).** The rows marked *pending* above follow `specs/07` §5.1, `specs/14` §4.4 and `specs/15` §9.2. Exceptions to the rules of this section: (1) every NIfTI under `stimulations/` is uncompressed `.nii`, not `.nii.gz`, because Lead-DBS matches these names with a `.nii` end anchor (`07` §5.1, `14` D12, `15` D7); readers also accept `.nii.gz` in imported Lead-DBS folders; a label folder may hold Lead-DBS outputs of other models, which are left alone (`07` §10). (2) Lead Group files live outside the subject tree, in the group folder `<dataset>/derivatives/leadgroup/<guid>/` (`14` §2.3, §11: `dataset-<dataset>_analysis-<guid>.json` primary record, `.mat` twin, `exports/`, `sweetspots/<id>/`, `fiberfiltering/<id>/`, `networkmapping/<id>/`, `viz/scene_manifest.json`), which does not move with `RunOptions.output_dir`. Labels starting with `gs_` are reserved for group stimulations (`07` §2.6 rule 10).

**Method logs are written by the stage** (not only by the QC viewer): every completed `coreg`, `normalize` and `brainshift` writes its method log through the staged write with `approval: 0` (a fresh result needs fresh QC) and the `04` §14 keys `method`, `engine` (`ants` or `wgpu`), `custom: false`, `version` (the `dbsloc` version) plus the stage's details: `normalize` = the former channel record (`norm_isotropic_mm`, template path, `template_space`, T2 template, channel list, backend, adapter); `coreg` = `{"CT": {...}, "<MR key>": {...}}` with method, backend and the transform name per image, and the per-modality `approval` dict of `03` §12.3.4. The approved-lock checks of `00` §6 that read the channel record read the method log instead.

**`Subject` bundles** (§6.4–6.5) are filled from these names at resolution time, for files that exist: `coreg.anat.preop[mod]`, `coreg.anat.postop[key]`, `coreg.transform[key]`, `norm.anat.preop[anchor]`, `norm.anat.postop[key]`, `norm.transform.forward` / `inverse`, `coreg.log.method`, `norm.log.method`. Consumers read these fields; no code globs the derivative directories for images (former `discover_norm_postop_niftis`).

**Parsing:** path ↔ entities needs **BIDS path parser** (conventional `parse_bids` / `set_entity` in Python); **round-trip** property tests required.

---

## 8. Import & classification (rawdata)

### 8.1 NIfTI commit rules (from unsorted or user files)

- At least **one** pre-op anat file.  
- If any post-op: at least one **CT** or **MRI**; if **MRI**: at least one `acq-ax`, `acq-cor` or `acq-sag` image (any one orientation suffices).  
- No duplicate (session, type, kind, acq) row.  
- **Acquisition guess from voxel** (only when the file name, compared case-insensitively as a substring, carries no `acq-ax` / `acq-cor` / `acq-sag` / `acq-iso`): when the largest voxel size is not positive, or the largest and smallest voxel sizes differ by at most 10 % of the largest, there is no guess. Otherwise the slice axis is the voxel axis whose size is farthest from the median of the three sizes (on a tie, the larger size). For the usual square in-plane pixels this is the axis normal to the plane of the two equal sizes, whether the slices are thicker or thinner than the pixels. Its direction (the sform column when `sform_code > 0` and the sform 3 × 3 part is not all zero, else the rotation of the qform quaternion) is mapped to the world axis with the largest absolute component, the first of x, y, z on a tie: x → `sag`, y → `cor`, z → `ax`. An isotropic image gets no `acq` (an `acq-iso` comes only from the file name); a post-op MR without an orientation, or with `acq-iso`, gets `ax`.

**Supported** pre-op **suffixes** (minimum): `T1w`, `T2w`, `PDw`, `T2starw`, `FLAIR`, `FGATIR`, (optional) `WMn`, etc. **Post-op:** `CT`, or MRI with an orientation (`acq-ax` / `acq-cor` / `acq-sag`).

### 8.2 DICOM pipeline (high level)

1. DICOMs under `sourcedata/sub-<id>/` (may include `DICOM/`).
2. Converter (e.g. dcm2niix) → `rawdata/.../unsorted/*.nii.gz` + JSON.
3. Classifier (GUI or rules from `dicom_bids_lookuptable` equivalent) → move into `ses-preop` / `ses-postop` / `anat` with BIDS names.
4. Regenerate `**rawimages.json`** (or equivalent) index for pipeline discovery.

**Failure:** conversion error → no file in rawdata; duplicate series → user resolution.

### 8.4 Import wizard — NIfTI files and DICOM folders into a subject (normative, 2026-10-14)

Users have loose files, not datasets. The desktop GUI's **Import images** wizard (and any other front end) turns them into the layout of §2–§4 through one shared module, `dbsloc_pipeline::import` (no pipeline logic in the GUI, `AGENTS.md` §3).

**Inputs.** A dataset root (existing or new folder), a subject id (`<id>` per §3, with or without `sub-`), and a list of **items**, each: source NIfTI (`.nii` or `.nii.gz`), optional JSON sidecar, `session` (`preop` | `postop`), `suffix` (pre-op: `T1w`, `T2w`, `PDw`, `T2starw`, `FLAIR`, `FGATIR`; post-op: `CT`, or an MR suffix from the same list), optional `acq` (`ax` | `cor` | `sag` | `iso`). DICOM folders are converted first (below) and their series become items.

Every row of the Images step can be previewed with its *View* button (single-volume preview, `specs/08_addendum_interactive_volume_viewer.md` §6.7), so a user can look at a converted series before deciding its session, type and orientation.

**Rules (plan time, before anything is written).**
- Subject id invalid → `E_BIDS_PARSE`. Source missing, not a NIfTI, or with an unusable header (dim[0] outside 3..=7, a zero spatial dimension, datatype 0, neither qform nor sform set) → `E_IMPORT_FILE`.
- Post-op MR items need `acq` (`ax`, `cor` or `sag`; none or `iso` → `E_IMPORT_RULES`); no particular orientation is required, a coronal-only or sagittal-only post-op MR set is valid (§8.1, §10 step 5). `CT` items never carry `acq` (a CT with `acq` → `E_IMPORT_RULES`). Pre-op items may carry `acq`.
- Duplicate (session, suffix, acq) among the items → `E_IMPORT_RULES` (the resolver would raise `E_DUPL_RAW`).
- A destination path longer than 240 characters is a plan warning (Windows `MAX_PATH`); staging names add a prefix.
- After the import the subject must resolve (§10), checked at plan time: when the items hold no pre-op image, the subject must already exist and already resolve to a pre-op anchor (a new subject, or an existing subject without one, → `E_NO_ANCHOR`, nothing written). The merged index (existing `rawimages.json` rows not replaced by the new rows, the new rows, then the rows of the Lead-DBS prefs index `prefs/sub-<id>_desc-rawimages.json` of §10 step 2) must have no two rows with equal `(session, datatype, entities)` → else `E_DUPL_RAW` at plan time, naming both index files. An existing `rawimages.json` that does not parse fails the plan (`E_IMPORT_FILE`).
- Destination image **or sidecar** exists already, or an existing index row points at a destination path → listed as a **conflict**; the import runs only with `replace_existing = true`, and the replaced file (and its index row) is restored on any later failure. Index rows with the same key *or the same path* as a new row are replaced, never duplicated.
- Source integrity: a `.nii` must hold at least `vox_offset + ∏dim · bitpix / 8` bytes; a `.nii.gz` is fully decoded while it is copied, so a truncated or corrupt stream fails (`E_IMPORT_FILE`) and is rolled back. (Amended 2026-10-25:) a `.nii.gz` source, including dcm2niix output, is copied byte for byte while it is decoded for this check, never re-compressed; a plain `.nii` source is compressed once into one gzip member by the parallel writer at level 3.

**Outputs.**
- `rawdata/sub-<id>/ses-<session>/anat/sub-<id>_ses-<session>[_acq-<acq>]_<suffix>.nii.gz` — always gzip (a `.nii` source is compressed on copy); the sidecar, when given, is copied next to it as `.json`.
- `rawdata/sub-<id>/rawimages.json` (§9): the existing rows are kept, rows with the same (session, datatype, entities) are replaced, one row per item: `{"path": "<relative to dataset root>", "session": "preop"|"postop", "datatype": "anat", "entities": {"suffix": "<suffix>"[, "acq": "<acq>"][, "modality": "CT"|"ax_MRI"|"cor_MRI"|"sag_MRI"]}}`; `modality` is written for post-op rows only (it is what §10 reads for the post-op bundle).
- `dataset_description.json` at the root when missing: `{"Name": "<root folder name>", "BIDSVersion": "1.9.0", "DatasetType": "raw"}`.
- Writes are staged (`.partial-*` names, then rename) and journaled; on error or cancel every created file is removed and every replaced file and the previous index are restored (same guarantee as `outputs.rs`).
- **Invalidation of a replacing import.** When the import replaced an existing raw image or index row, the stages that read it are invalidated as in `00` §6.1 (lock `approval: false`, method-log approval cleared), with `invalidated_by: "import"`: a replaced **pre-op** image → `bids`, `preprocess` and their transitive dependents; otherwise a replaced **post-op** image → `coreg` and its transitive dependents. The import report carries the result; the log line is `dbsloc: import replaced raw images; approvals invalidated: <stage>, …` (shown in the GUI log; the server returns the invalidated stages in its commit response). An import that only adds files invalidates nothing.

**DICOM conversion.** `dcm2niix` (MIT, §10 of `AGENTS.md`) resolved as `DBSLOC_DCM2NIIX_BIN` → bundled `resources/dcm2niix/<platform>/` → `PATH`; absent → `E_DCM2NIIX`, message "dcm2niix was not found: set DBSLOC_DCM2NIIX_BIN or install dcm2niix on PATH". On failure or cancel the converter's output directory is emptied again; series are listed by series number, then name. A folder counts as DICOM when any file below it has a `.dcm` / `.ima` extension or the `DICM` magic at byte 128. Invocation: `dcm2niix -z y -b y -i y -f %f_%p_%z_%t_%s_%d -o <temp out dir> <folder>`, cancellable (the process is killed), no network. Each produced NIfTI becomes a **converted series** with its sidecar's `SeriesNumber`, `SeriesDescription`, `ProtocolName`, `Modality` (`MR` | `CT`), the voxel size and dimensions from the header, and a **suggestion**: `Modality == CT` → post-op `CT`; otherwise suffix from the description (`t2` → `T2w`, `flair` → `FLAIR`, `pd` → `PDw`, `t2*` / `swi` → `T2starw`, `fgatir` → `FGATIR`, else `T1w`), session `preop`; `acq` from the voxels (§8.1: largest and smallest size differing by at most 10 % of the largest → none; else the voxel axis whose size is farthest from the median of the three sizes, on a tie the larger, mapped through the affine to the world axis it mostly follows: z → `ax`, y → `cor`, x → `sag`). Suggestions are editable; nothing is written until the import runs. Sidecars come from dcm2niix's default BIDS anonymisation (`-ba y` is its default with `-b y`: no patient name, id, birth date or other identifying tags), so copying them next to the image adds acquisition parameters, not identity; the server's header scrub (`descrip` / `aux_file`) is a server-side rule on top of this.

**GUI (`crates/dbsloc-app`, `docs/agent-rules/gui-ux.md`).** Three steps, Back / Next / Import in a pinned bar, Cancel always available, a tooltip on every control: (1) *Destination* — dataset folder (Browse…), subject id with live validation and an "adds to existing subject" note; (2) *Images* — rows (source, session, type, orientation, remove), *Add NIfTI files…*, *Add DICOM folder…* (conversion runs on a worker with elapsed time and Cancel), rule violations listed under the table; (3) *Review* — planned destination names, conflicts with a *Replace existing files* switch, progress (`n / total` files, current name) during the import. Success opens the imported subject; failure shows the error window with Copy.

**Amendments 2026-10-22 (`specs/13` Part 1 parity; normative).**
- **dcm2niix** runs with `-z y -b y -i y -f %f_%p_%z_%t_%s_%d` (localizers / derived / 2-D series ignored; the input folder, protocol, sequence, date-time, series number and description in the name).
- **Series metadata** (`SeriesInfo`, from the sidecar, else the file name): series number, description, protocol, modality, `acquisition_datetime` (`AcquisitionDateTime`, else `AcquisitionDate` / `SeriesDate` / `ContentDate` / `StudyDate` with their times, else a 14-digit date-time token in the file name), `study_date`, image type, manufacturer, field strength, TE / TR / TI, slice thickness, dimensions, voxel size. Missing dates are allowed.
- **Lookup table** (`LookupTable`, default `default_lookup_table()`, replaced by `DBSLOC_IMPORT_LOOKUP_JSON` or a GUI-supplied table; JSON `[{"session", "suffix", "keywords"}]`): rules in order, **the last matching rule wins**: preop `T1w` ← `t1`, `mprage`; `T2w` ← `t2`, `tse`; `FGATIR` ← `fgatir`; `PDw` ← `pd`; `T2starw` ← `t2star`, `t2*`, `swi`; `FLAIR` ← `flair`; postop `CT` ← `ct`. Keywords are searched case-insensitively in SeriesDescription, ProtocolName and the file stem (the input-folder prefix removed); a keyword of up to three letters / digits matches a whole word optionally followed by `w` (`t1` matches `T1w`, not `direct`), longer keywords match anywhere (plain substrings, no regular expressions). A sidecar `Modality` of `CT` always means post-op CT; any other value disables the CT rule. A post-op MR without an orientation keyword gets `ax`.
- **Unassigned series** (no rule matched) are kept in the list with `assigned = false` and are not imported unless the user assigns them (`dbsloc import` stops with exit 2 and lists them unless `--auto` leaves them out).
- **Study-date rule** (opt-in: `--postop-latest-study`, the wizard button): series are grouped into studies by `study_date` (oldest first, undated last); with two or more dated studies the assigned pre-op non-CT series of the latest study become post-op MR.
- **Source archive:** every input DICOM folder is copied to `sourcedata/sub-<id>/DICOM/<folder name>[-N]/` through a `.partial-*` folder, journaled and removed on rollback (`keep_sourcedata`, default on; a folder already under that subject's `sourcedata` is skipped with a warning).
- **Subject id from a folder** (`subject_id_from_folder`): a leading `sub-` is stripped, everything but `[A-Za-z0-9]` removed, with a warning when characters were dropped.
- `dataset_description.json` gains `GeneratedBy: [{Name: "Wyss DBS Localizer", Version: <version>}]` when created or when an existing object lacks it (key order kept).
- **Batch import** (`plan_import_batch` / `run_import_batch`): several subjects one after another, each with its own journal; a duplicate id is `E_IMPORT_RULES`; one subject's failure leaves the others.
- **Headless command** `dbsloc import` (`docs/RUN_REFERENCE.md`).
- **GUI wizard (reworked, replaces the three steps above):** (1) *Images* — *Add folders…* (several; a folder with NIfTI files at its top level is added as is, any other folder is converted as DICOM on the cancellable worker, one folder at a time, with the active lookup table), *Add NIfTI files…*, *Post-op = latest study*; an input list (kind, subject id derived from the folder name with its warning, image count, remove); the series table grouped by study (oldest first, undated last) with tick, series number, description, time, modality, voxel size, session, type, orientation (post-op MR: ax / cor / sag, axial by default; CT none) and remove; unassigned rows start unticked and greyed, editing a combo ticks the row; rule errors from a dry-run plan show live and block *Next* (`E_NO_ANCHOR` is ignored on this step). (2) *Subject and destination* — subject id prefilled from the first input folder (a folder named `DICOM` uses its parent), dataset folder (default: the open or last-used dataset), the per-dataset output folder read-only, *Keep a copy of the source DICOM in sourcedata* (on; shown when a DICOM input has ticked rows), *Replace existing images* (when the subject exists), and the post-op modality to use (Automatic / CT / MRI; shown only when both a post-op CT and a post-op MR are ticked; stored as the GUI preference that applies to subjects with both modalities). (3) *Review* — the dry-run plan (planned files, replacements, `sourcedata` copies, warnings, the number of unticked images left out), `E_NO_ANCHOR` blocks, no post-op image (and none on disk) asks *Import anyway?*, then import with progress, rollback and success opening the subject. **Several subjects:** when the input folders give different ids, *One subject per input folder* (on by default) imports them with `plan_import_batch` / `run_import_batch` (same-id inputs merged, subjects without ticked rows dropped, subjects whose plan failed skipped and reported), per-subject progress and a result list; no per-subject assignment editing. Screenshot mode: `DBSLOC_APP_SCREENSHOT_ACTION=import` with `DBSLOC_APP_IMPORT_INPUTS` (paths separated by the OS path separator), `DBSLOC_APP_IMPORT_ROOT`, `DBSLOC_APP_IMPORT_LATEST_STUDY`, `DBSLOC_APP_IMPORT_STEP=subject|review`.

**Import and runs are exclusive in the GUI (2026-11-12, normative).** An import replaces raw images, rewrites `rawimages.json` and invalidates locks (above); a pipeline run reading the same subject at the same time would certify results computed from the old images. The same holds for the other GUI jobs that write stage results, approvals or locks of a subject: a registration review (its decision writes an approval; a clean-up runs in the background afterwards), *Refine localization* (a saved correction) and the group analysis *Compute VTAs* job (the `vat` stage per patient). Not part of this lock, because they write neither raw images, `rawimages.json` nor stage outputs, approvals or locks: stimulation *Save* (the label's settings JSON, `07` §2.3; a changed JSON makes the label's VTA not up to date, `07` §7.2), stimulation *Compute connectivity* and the group *Analysis* jobs (outputs under the label's `stimulations/` folder or the group folder, each with a provenance sidecar whose input checksums mark it stale when its inputs change, `15` §9.3 and §8 *Re-use*; group results record each label's `settings_sha256` and output sha256, `07` §7.6), and the session-report import (`clinical/reports/`, stimulation labels and `sourcedata/sub-<id>/reports/`, `16` §5.1). Therefore: (a) the wizard does not open while a run, a run of several subjects (`specs/13` §9), a registration review being prepared or open or its clean-up still running, *Refine localization* being opened, open or saving, or the group *Compute VTAs* job is active (until its worker has really ended, also after the group was closed), from any entry point (header *Import… → Images into a subject…*, `Ctrl+I`, the start screen, the guided Subject step); the menu item and the buttons are disabled with the reason as hover text, and `Ctrl+I` shows it in the error window; (b) while the wizard is open the check is repeated: *Import* is disabled with the reason, and a request that still arrives (*Import*, *Import anyway?*) is refused with the same reason in the wizard's error window; nothing is written and the wizard stays open with every choice kept; (c) while the import job is writing into the dataset, none of those jobs starts: a single run is refused and *Run* is disabled with "Wait until the image import has finished writing into the dataset.", a batch does not start and its next subject waits (it resumes on its own), and preparing a registration review, opening *Refine localization*, saving a correction and the group *Compute VTAs* are refused with that reason; closing the application asks first. DICOM conversion into the temporary folder holds back nothing: it writes nothing into the dataset. Details: `specs/08` §11.1.9.

### 8.3 Legacy (non-goal for Wyss DBS Localizer v1 I/O)

**Optional** adapter: flat `prenii_unnormalized` / `postop_ct` names in a patient folder → migrator emits BIDS (see `phase_a` legacy section).

---

## 9. Index file (`rawimages.json` or equivalent)

- **Location:** e.g. `rawdata/sub-<id>/rawimages.json` (exact path in implementation; must be **one** canonical index).  
- **Content:** list of {path, session, datatype, entities} for all raw NIfTIs.  
- **Regenerated** when rawdata set changes. **Runner** uses it + filesystem to build `Subject`.

---

## 10. Algorithm: resolve `Subject` from disk

**Input:** `dataset_root`, `subj_id`, `prefs` (prenii order, spacedef handle).  
**Output:** `Subject` or error.

1. **Validate** `subj_id`.
2. **Load** `spacedef` for active template space.
3. **Walk** `rawdata` + `derivatives/leaddbs` to fill **all existing** path fields.
4. **Compute** `anchor_modality` and `postop_modality` from index + `prenii_order` + `norm_mapping`.
5. **Set** `primary_template` = `spacedef.norm_mapping[strip_acq(anchor_key)]` else `misfit_template`.
6. **If** no anchor raw path → **error** `E_NO_ANCHOR`.
7. **Return** struct.

**Idempotent:** reread = same if disk unchanged.

**Wyss DBS Localizer implementation** (`crates/dbsloc-pipeline/src/bids.rs` `resolve_subject_with`, the only resolver since unification phase 2; Python reaches it as `dbsloc_reconstruct.resolve_subject`):

1. `subj_id` with or without `sub-`; `sub-<id>` must match `^sub-([^\W_]+)$` (alphanumeric `id`) → else `E_BIDS_PARSE`. `rawdata/sub-<id>/` missing → `E_NO_SUBJ`.
2. **Index** (unless `--no-index`): rows of `rawdata/sub-<id>/rawimages.json` (list of `{path, session, datatype, entities}`, `path` relative to the dataset root) followed by the Lead-DBS prefs index `derivatives/leaddbs/sub-<id>/prefs/sub-<id>_desc-rawimages.json` (`{preop|postop: {anat: {<logical key>: <file stem>}}}`, a stem resolved to `ses-<session>/anat/<stem>.nii.gz`, else `.nii`, else the first file, by the ordering rule below, whose name starts with the stem). Prefs keys map to suffixes (`…_T1w` / `T1w` → `T1w`, `CT` → `CT`, …). Two rows with equal `(session, datatype, entities)` (entities compared key-sorted) → `E_DUPL_RAW`. Malformed index JSON → `E_BIDS_PARSE`. Rows whose file does not exist are ignored; the first row per pre-op suffix / post-op bundle key wins.
3. Pre-op modalities from the index; if none, from `ses-preop/anat` files ending `_<suffix>.nii[.gz]` for suffix in `T1w, T2w, PDw, T2starw, FLAIR, FGATIR`, the first path per suffix by the ordering rule below. Anchor = first of that list present, else the case-insensitively first key; none → `E_NO_ANCHOR`.
4. Post-op bundles: index rows, then `ses-postop/anat` files by the ordering rule (first per key wins): `_ct.nii[.gz]` → `CT`, `acq-cor` → `cor_MRI`, `acq-sag` → `sag_MRI`, otherwise `ax_MRI` (all matches ignore case). A file that an index row already placed in a bundle is not added again under a second key. An index row's bundle key: logical `CT` or a file name containing `_ct.nii` → `CT`; else the file name's `acq-cor` / `acq-sag` / `acq-ax` → `cor_MRI` / `sag_MRI` / `ax_MRI`; else the logical key `cor_MRI` / `sag_MRI` (case-insensitive) → that key; else `ax_MRI`. For an index row the file-name tests also accept the tokens `_cor_` / `_sag_` for `cor_MRI` / `sag_MRI`. The logical key is the row's `modality` entity. When an index row has no `modality` entity, or a null, non-string or blank one (hand-written indexes), its logical key comes from the row itself: `CT` when its `suffix` is `CT` (case-insensitive), else `<acq>_MRI` when its `acq` is `ax` / `cor` / `sag`, else none, so the file name decides as above (default `ax_MRI`). A post-op row never defaults to `CT`. The bundles keep this insertion order (index rows first, then files); it is not re-sorted, and `postop_raw` iterates in it.
5. `postop_modality` follows the resolved bundles of step 4 (file-name tests ignore case): **CT present** = the post-op bundle has a `CT` key (from an index row or a file); **MR present** = `ses-postop/anat` holds a file whose name contains `acq-ax`, `acq-cor` or `acq-sag`, or an index row (either index of step 2) produced an `ax_MRI`, `cor_MRI` or `sag_MRI` bundle. Any one post-op MR orientation suffices: a subject whose only post-op MR is coronal or sagittal is a post-op MRI subject. A post-op file without an `acq` orientation and without an index row still enters the `ax_MRI` bundle (step 4) but does not count as MR present. `CT` when only CT is present, `MRI` when MR is present (**CT + MR → `MRI`**), `None` otherwise; `both_mrct_present` = both. Reconstruction takes the CT path only for `CT`; on `MRI` it reconstructs in template space from the first of `ax_MRI` > `cor_MRI` > `sag_MRI` that has a normalized image (`05` §3, `Subject::norm_postop_mr_in_use`). (This resolves the former inconsistency between steps 4 and 5, where step 5 looked at `ses-postop/anat` file names only and ignored a CT or MR listed in an index under another name; until 2026-10-16 only an axial MR counted.) **Choice when both exist (2026-10-22):** `PostopModality::Auto` (default) picks **CT** when a CT exists, else MRI; when both exist a stored `uiprefs.mat` `modality` of 1 (MR) picks MRI (2, 3, anything else or an unreadable file → CT; a stored MR choice without an MR image is ignored). `Ct` / `Mri` (`RunOptions.postop_modality`, `DBSLOC_POSTOP_MODALITY`) force one and fail with `E_MISS_POSTOP` when that image is missing; the GUI passes its preference only for a subject that has both modalities and `Auto` otherwise (no per-subject store: Lead-DBS keeps the choice in `uiprefs.mat`, which is read, not written). Until 2026-10-22 the resolver picked MRI when both existed.
6. Derived paths, all named by `derivatives.rs` per §7a and filled into the `Subject` bundles when the files exist: preprocessed anchor, precoreg anchor, the anchor in use (`02` §6.1: precoreg > preprocessed > raw), coregistered post-op images and their transforms, the normalized anchor and post-op images and the normalization composites (only for the template space recorded in the normalization method log), second SyN channel (`04` §5.2) = the pre-op `T2w` of step 3 unless the anchor is that `T2w`, and its rigid coregistration, the method logs, `locks/`, `prefs/sub-<id>_desc-uiprefs.mat`. The dataset root is canonicalized (Windows `\\?\` prefix stripped).
7. Subject listing (`dbsloc subject list`, the GUI) collects `sub-<id>` directories under `rawdata/`, `sourcedata/`, `derivatives/` and `derivatives/leaddbs/`, deduplicated and sorted.

**Ordering rule.** Wherever the resolver picks one file among several (pre-op file per suffix, post-op file per bundle, prefs stem match, first NIfTI in a directory, normalized-CT candidates, checkreg `*_Warped.nii.gz` pairs) it sorts by the key (lower-cased path, original path) and takes the first; the result is therefore the same on case-sensitive and case-insensitive file systems and with any directory-listing order. Directory walks do not descend into symbolic links or junctions to directories, and skip `.partial-*` files (in-progress outputs, `00` §5.2).

---

## 11. Edge cases and errors


| Code            | When                                  |
| --------------- | ------------------------------------- |
| `E_NO_SUBJ`     | subject folder missing                |
| `E_NO_ANCHOR`   | no usable pre-op anat                 |
| `E_BIDS_PARSE`  | filename fails entity parse           |
| `E_DUPL_RAW`    | duplicate classification row          |
| `E_MISS_POSTOP` | the requested post-op modality (`ct` / `mri`, §10 step 5) has no raw image |
| `E_IMPORT_FILE` | import item is missing, not a NIfTI, or has an unusable header (§8.4) |
| `E_IMPORT_RULES` | import items break §8.1 / §8.4 rules (post-op MR without an `ax` / `cor` / `sag` orientation, CT with `acq`, duplicates) |
| `E_DCM2NIIX` | DICOM conversion: converter not found, failed, or produced no series (§8.4) |
| `E_IMPORT_CANCELLED` | the user cancelled an import or conversion; everything written so far is rolled back |


**Partial datasets:** pre-op only (no postop) — allowed; `postop_modality` = `None`, skip CT/MRI post-op paths.

---

## 12. Acceptance tests

- Golden **minimal** `rawdata` + `derivatives` tree → `Subject` JSON snapshot matches fixture.  
- `anchor_modality` picks T1w when T1+T2 present.  
- Transform **bases** end with `…_desc-` and join correctly with `ants.nii.gz` and `ants.mat` (mock).  
- BIDS name **round-trip** for 20 representative filenames.  
- `rawimages` index regeneration: add file → new entry.

---

## 13. References

- BIDS 1.6+ specification; BIDS **derivatives** (raw).  
- Lead-DBS public docs: [https://netstim.gitbook.io/leaddbs](https://netstim.gitbook.io/leaddbs) (dataset prep).  
- Horn et al. (2019) for pipeline context only.

---

*End of spec 01.*