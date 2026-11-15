# Spec 01 — Dataset layout, subject resolution and image import

**Status:** Implemented: dataset and output-folder layout, subject resolution, derivative names and the import of NIfTI files and DICOM folders (library, GUI wizard, `dbsloc import`); not implemented: a writer for the reserved brainshift image names (§7a); migration of legacy flat patient folders is a non-goal (§8.3).
**Depends on:** `00_overview.md` (stages, staged writes `00` §5.2, locks and invalidation `00` §6); `13_import_and_batch_reference.md` Part 1 (Lead-DBS import behaviour, informative).

---

## 1. Purpose and scope

This spec is the contract for:

- where a dataset keeps raw images and pipeline outputs (§2–§5);
- how a subject is resolved from disk into the `Subject` record that every stage reads (§6, §9, §10);
- how every registration and reconstruction output is named (§7a, normative);
- how loose NIfTI files and DICOM folders become a subject (§8).

| Area | Code |
| --- | --- |
| Subject id, raw image indexes, resolver, subject listing | `crates/dbsloc-pipeline/src/bids.rs` |
| Dataset root and output root | `crates/dbsloc-pipeline/src/paths.rs` (`fn absolute_dataset_root`, `fn derivatives_root`) |
| Entity parser and derivative names | `crates/dbsloc-pipeline/src/derivatives.rs` |
| Import planning and writing, DICOM conversion, suggestions | `crates/dbsloc-pipeline/src/import.rs` |
| GUI *Import images* wizard | `crates/dbsloc-app/src/import_wizard.rs` |
| Commands | `crates/dbsloc-cli/src/cmd/import.rs` (`dbsloc import`), `crates/dbsloc-cli/src/cmd/subject.rs` (`dbsloc subject show`) |

---

## 2. Dataset root and output folder

```
<dataset>/
  dataset_description.json      written by the import when missing (§8.4.2)
  rawdata/sub-<id>/
    rawimages.json              raw image index (§9.1)
    ses-preop/anat/             pre-op images
    ses-postop/anat/            post-op images
  sourcedata/sub-<id>/
    DICOM/<folder>/             archived DICOM input (§8.4.2)
    reports/<vendor>/           session-report PDFs (`16`)
  derivatives/
    leaddbs/sub-<id>/           subject outputs under the default output root (§5)
    leadgroup/<guid>/           Lead Group analyses (`14` §2.3)
```

- **Output root.** `paths::derivatives_root(dataset, output_dir)` returns `output_dir` when it is set and not empty (made absolute; a relative path resolves against the current directory), else `<dataset>/derivatives/leaddbs`. `output_dir` comes from `RunOptions.output_dir`, `--output-dir`, `DBSLOC_OUTPUT_DIR` (flag > variable > default, `docs/RUN_REFERENCE.md`) or the GUI's *Output directory* row (per dataset; the import wizard shows it as `Pipeline output directory: …`). Everything under `sub-<id>/` moves with it: stage outputs, locks, `uiprefs.mat` and the Lead-DBS raw image index (§9.2). Raw images are always read from `<dataset>/rawdata`; the Lead Group folder stays under `<dataset>/derivatives/leadgroup/`. The pipeline, the commands and the import derive the root with `fn derivatives_root`; the GUI thread uses `fn derivatives_root_lexical`, the same rule as a pure path join without touching the file system.
- **Dataset root.** Canonicalized (`fn absolute_dataset_root`; the Windows `\\?\` prefix is removed by `bids::simplify_verbatim`), so every path derived from it has one spelling. A root that does not exist is made absolute instead.
- Resolution needs only `rawdata/sub-<id>/`; `dataset_description.json` is not required.

**Coordinates.** World coordinates are NIfTI world coordinates in millimetres (RAS). ITK transforms written by ANTs or the in-process engine act on LPS physical points; the conversions are specified where they are used (`02`, `04`, `05`).

**Voxel → world affine (normative, every NIfTI reader):** use the **sform** (`srow_x/y/z`) when `sform_code > 0`. Otherwise use the **qform** when `qform_code > 0`: rotation from the quaternion (`b, c, d` = `quatern_b/c/d`, `a = sqrt(max(0, 1 − b² − c² − d²))`), columns scaled by `pixdim[1..3]`, the third column also multiplied by `qfac = pixdim[0]` (taken as `1` unless it is `−1`), and translation `qoffset_x/y/z` (NIfTI-1 standard, method 2). Only when both codes are `0`, fall back to `diag(pixdim[1..3])` with zero origin (method 1). This matches nibabel's `get_best_affine`. Ignoring the qform misplaces any qform-only image by its full origin and rotation, which is wrong for centroids, coregistration starts (`03` §5.1) and every world-coordinate sample. Shared implementation: `fastwarp/crates/fastwarp-core/src/nifti_affine.rs` (`fn voxel_to_world`).

---

## 3. Subject id and subject listing

- On disk a subject is the folder `sub-<id>`. `<id>` is one or more letters or digits (`char::is_alphanumeric`, so non-ASCII letters pass); `_`, `-`, spaces and every other character are refused (`bids::parse_subj_id`).
- Every entry point accepts the id with or without `sub-` and trims surrounding white space.
- An invalid id fails with `E_BIDS_PARSE`: `not a valid sub-<id> folder name: "<sub-…>"`.
- Ids proposed from folder names by the import keep only `A–Z`, `a–z` and `0–9` (§8.4.5).

**Listing.** `bids::list_subject_ids_in(dataset, output_dir)` returns the names of the directories that start with `sub-` (and are longer than that) under `rawdata/`, `sourcedata/`, `derivatives/` and the output root, without `sub-`, de-duplicated and sorted as strings. Names are not validated, so a listed subject can still fail to resolve. Used by the GUI subject list, `dbsloc run --all-subjects` (`batch::expand_subjects`) and the server's subject list (default output root).

---

## 4. Sessions, modalities and keys

- **Sessions:** `ses-preop` and `ses-postop` (fixed lower-case literals) under `rawdata/sub-<id>/`; images live in `anat/` (subfolders of `anat/` are searched too). Other sessions and datatypes (`func/`, `dwi/`) are ignored.
- **Pre-op modality** = the BIDS suffix of the file. Recognised suffixes, in anchor priority order (`bids.rs` `ANCHOR_PRIORITY`, `import.rs` `PREOP_SUFFIXES`): `T1w`, `T2w`, `PDw`, `T2starw`, `FLAIR`, `FGATIR`.
- **Anchor modality** = the first suffix of that list among the subject's pre-op modalities; when none of them is present but a pre-op index row names another suffix, the case-insensitively first such suffix (`fn select_anchor_modality`). `E_NO_ANCHOR` arises only when there is neither a usable pre-op index row nor a scanned file with a recognised suffix (§10 step 3). It is a bare suffix (`T1w`): `acq-` and other entities stay in the file name.
- **Post-op bundle keys:** `CT`, `ax_MRI`, `cor_MRI`, `sag_MRI` (`bids::POSTOP_MR_KEYS` order: ax, cor, sag). A post-op MR may carry any suffix of the pre-op list.
- **Post-op modality:** `CT`, `MRI` or none (§10 step 5).

---

## 5. Subject derivative tree

`subj_dir` = `<output root>/sub-<id>`. Folders the code writes or reads:

| Folder | Content | Specified in |
| --- | --- | --- |
| `preprocessing/anat/` | N4-corrected anchor and its record | `02` |
| `coregistration/anat/`, `transformations/`, `log/` (with `log/multistart/`), `checkreg/` | precoreg anchor, images in anchor space, transforms, method log, QC images | `02` §6, `03` |
| `normalization/anat/`, `transformations/`, `log/`, `checkreg/`, `work/` | images in template space, warps, method log, QC images; `work/` is scratch | `04` |
| `brainshift/log/`, `brainshift/checkreg/` | brainshift method log and QC images | `03` §12.4a, `00` §5.2 |
| `reconstruction/`, `reconstruction/viz/` | reconstruction record and its `.mat` twin, PaCER brain masks, 3D scene manifest | `05`, `08` |
| `stimulations/<space>/<label>/` | stimulation settings, VTAs, connectivity outputs | `07`, `14`, `15` |
| `headmodel/native/` | E-field basis cache | `07` §3.6 |
| `clinical/reports/` | imported session reports | `16` |
| `prefs/` | Lead-DBS files, read and never written: `sub-<id>_desc-uiprefs.mat` (electrode model, post-op choice), `sub-<id>_desc-rawimages.json` (§9.2) | — |
| `locks/` | stage locks | `00` §6 |
| subject folder root | `sub-<id>_desc-stats.mat` and its backup | `07` §8.4, `14` §3 |

---

## 6. The resolved subject record

`bids::Subject` is what every stage, the GUI and the server read. Every path is absolute. A field for a derived file is set only when that file exists at resolution time; `Subject::refresh_bundles` re-reads them after a stage.

`OrderedPaths` is a list of `(key, path)` pairs in insertion order; inserting a key that is already present keeps the first path.

### 6.1 Identity and folders

`subj_id` is the id without `sub-`; `subj_dir` = `<output root>/sub-<id>`; `rawdata_dir` and `sourcedata_dir` = `<dataset>/rawdata/sub-<id>` and `<dataset>/sourcedata/sub-<id>` (the latter need not exist); every other `<x>_dir` = `<subj_dir>/<folder>`: `preproc_dir` `preprocessing/anat`, `coreg_dir` `coregistration/anat`, `norm_dir` `normalization/anat`, `brainshift_dir` `brainshift/anat`, `recon_dir` `reconstruction`, `prefs_dir` `prefs`, `log_dir` `log`, `stim_dir` `stimulations`.

### 6.2 Raw images and modalities

| Field | Type | Value |
| --- | --- | --- |
| `anchor_modality` | `String` | suffix of the anchor (§4) |
| `postop_modality` | `Option<String>` | `"CT"`, `"MRI"` or `None` (§10 step 5) |
| `both_mrct_present` | `bool` | a post-op CT and a post-op MR are both present (§10 step 5), whatever the choice |
| `preop_raw` | `OrderedPaths` | suffix → raw pre-op file (§10 step 3) |
| `postop_raw` | `OrderedPaths` | bundle key → raw post-op file, in insertion order (§10 step 4) |
| `preop_anchor_raw` | `Option<PathBuf>` | `preop_raw[anchor_modality]` |
| `preop_anchor_preproc` | `Option<PathBuf>` | the N4-corrected anchor (§7a), when it exists |
| `postop_ct_raw` | `Option<PathBuf>` | `postop_raw["CT"]` |

### 6.3 Derived accessors

| Method | Returns |
| --- | --- |
| `anchor_in_use()` | the precoreg anchor when it exists, else the N4-corrected anchor, else the raw anchor (`02` §6.1) |
| `anchor_best()` | the N4-corrected anchor, else the raw anchor |
| `preop_t2_nonanchor()` | `preop_raw["T2w"]` unless the anchor is `T2w` (second SyN channel, `04` §5.2) |
| `postop_mr_images()` | the raw post-op MR files that exist, in the order `ax_MRI`, `cor_MRI`, `sag_MRI` |
| `norm_postop_mr_in_use()` | the first of `ax_MRI`, `cor_MRI`, `sag_MRI` in `norm.anat_postop` (`05` §3) |
| `has_postop_ct()` | `postop_ct_raw` exists |
| `locks_dir()`, `uiprefs_path()`, `reconstruction_json()` | `<subj_dir>/locks`, `prefs/sub-<id>_desc-uiprefs.mat`, `reconstruction/sub-<id>_desc-reconstruction.json` |

### 6.4 Coregistration bundle `coreg`

`CoregBundle`, filled from the §7a names for files that exist:

| Field | Keys → file |
| --- | --- |
| `anat_preop` | anchor suffix → precoreg anchor; `T2w` → the T2w in anchor space (only when `preop_t2_nonanchor()` is set) |
| `anat_postop` | `CT` and every MR key of `postop_raw` → that image in anchor space |
| `transform` | `T2w`, `CT` and the MR keys → their `.mat` affine to `anchorNative` |
| `log_method` | `coregistration/log/sub-<id>_desc-coregmethod.json` |

The CT composites (`.h5`) are not in the bundle; their names come from `derivatives::coreg_ct_forward` / `coreg_ct_inverse`.

### 6.5 Normalization bundle `norm`

`NormBundle`, filled only for the template space recorded in the normalization method log:

| Field | Value |
| --- | --- |
| `log_method` | `normalization/log/sub-<id>_desc-normmethod.json` |
| `template_space` | the log's `template_space`; when missing, every field below stays empty |
| `anat_preop` | anchor suffix → anchor in template space |
| `anat_postop` | `CT` and every MR key of `postop_raw` → that image in template space |
| `transform_forward`, `transform_inverse` | the `.h5` composites anchorNative → template and back |

The linear parts (`desc-affine` `.mat`) are not in the bundle; their names come from `derivatives::norm_forward_affine` / `norm_inverse_affine`.

### 6.6 JSON form

`Subject::to_json` serializes the identity, the modalities, the raw maps and the main folders and files; the Python extension returns it from `dbsloc_reconstruct.resolve_subject`, and `dbsloc subject show` prints a subset (§10.2).

---

## 7. File names

**Entities** (`derivatives::Entities`). A file name is parsed by removing `.nii.gz`, `.nii` or `.json` (case-insensitive) and splitting the stem at `_`. A token of the form `key-value` with both parts non-empty becomes an entity pair, in order; the last token is the suffix unless it is such a pair; any other token (also `a-` or `-a`) is kept as a bare entry. `Entities::stem` writes the entries back in order, so parsing and writing a BIDS name gives the same name. `Entities::with(key, value)` removes `key` wherever it is and appends it after the remaining entries.

### 7a. Derivative names (normative, 2026-10-12)

**Ownership.** Every name below is relative to `<output root>/sub-<id>/` (§2). `crates/dbsloc-pipeline/src/derivatives.rs` names every output of the stages `preprocess` to `reconstruct` and every file the resolver looks for. No other code formats these names, and the `Subject` bundles (§6.4, §6.5) are filled from them, never by globbing the derivative folders. The stimulation, group, connectome and session-report modules name their own outputs (second table). Older names are not read; a tree written by an older version is re-run.
Implementation status: names built by hand instead of through `derivatives.rs`: `sub-<id>_desc-reconstruction.json` in `crates/dbsloc-pipeline/src/group/` (`calc.rs`, `view.rs`, `sweetspot.rs`, `lead_group_mat.rs`), `crates/dbsloc-pipeline/src/tutor.rs`, `crates/dbsloc-app/src/group_panel.rs` (`fn reco_file`) and `crates/dbsloc-cli/src/cmd/group.rs` (the `has_reconstruction` check); `reconstruction/viz/scene_manifest.json` in `crates/dbsloc-app/src/panels.rs` (`fn default_manifest_path`); `crates/dbsloc-server/src/routes.rs` (`fn viz_scene`) also reads the older `<subj_dir>/viz/scene_manifest.json`.

**Image rule.** A derivative image of a raw image keeps the raw file's entities in their order and appends `space-<S>` and then `desc-<D>` before the suffix; an entity that is already present is removed from its place first, so it is never duplicated: raw `sub-01_ses-postop_acq-ax_T2w.nii.gz` → `sub-01_ses-postop_acq-ax_space-anchorNative_desc-preproc_T2w.nii.gz`. `<S>` is `anchorNative`, the template's space id `<Space>` (`04` §3.1, e.g. `MNI152NLin2009bAsym`) or `CT`; there is no `res-` entity (the method log records the template). Every derivative image is `.nii.gz`, whatever the raw extension (exception: stimulation images, below). When a raw image is missing, names are formed from a fallback raw name (`sub-<id>_ses-preop_<anchor>.nii.gz`, `sub-<id>_ses-preop_T2w.nii.gz`, `sub-<id>_ses-postop_CT.nii.gz`, `sub-<id>_ses-postop_acq-<o>_T1w.nii.gz`), so every name is defined.

**Transform rule** (fMRIPrep layout, `fn transform_name`): `sub-<id>[_ses-<s>][_acq-<a>]_from-<A>_to-<B>[_desc-<D>]_mode-image_xfm.<ext>`. `<A>` / `<B>` are a raw suffix (`T1w`, `T2w`, `CT`, …), `anchorNative` or `<Space>`; `<ext>` is `mat` (ITK affine as written by ANTs or the in-process engine) or `h5` (ITK composite). External tools receive short `.partial-*` work names in the target directory; their fixed output suffixes (`0GenericAffine.mat`, `Composite.h5`, `InverseComposite.h5`, `_Warped.nii.gz`) are renamed to these names when the stage commits (`00` §5.2).

**Stage outputs** (`<mod>` = anchor suffix; `<raw … entities>` = the raw file's name without extension and suffix):

| Stage | Output | Name |
| --- | --- | --- |
| preprocess | N4-corrected anchor + record | `preprocessing/anat/<raw anchor entities>_desc-preproc_<mod>.nii.gz` + `….json` |
| precoreg | reheadered anchor | `coregistration/anat/<raw anchor entities>_desc-precoreg_<mod>.nii.gz` |
| precoreg | transform | `coregistration/transformations/sub-<id>_ses-preop_from-<mod>_to-<Space>_desc-precoreg_mode-image_xfm.mat` |
| precoreg | record | `coregistration/log/sub-<id>_desc-precoreg.json` |
| coreg | CT in anchor space | `coregistration/anat/<raw CT entities>_space-anchorNative_desc-preproc_CT.nii.gz` |
| coreg | CT transforms | `coregistration/transformations/sub-<id>_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat`; composites `…_from-CT_to-anchorNative_mode-image_xfm.h5` and `…_from-anchorNative_to-CT_mode-image_xfm.h5` when written |
| coreg | multi-start record + work files | `coregistration/log/sub-<id>_ses-postop_desc-multistart_CT.json`; work files in `coregistration/log/multistart/` |
| coreg | post-op MR in anchor space | `coregistration/anat/<raw MR entities>_space-anchorNative_desc-preproc_<suffix>.nii.gz` (`<suffix>` = the raw file's suffix) |
| coreg | post-op MR transform | `coregistration/transformations/sub-<id>_ses-postop_acq-<a>_from-<suffix>_to-anchorNative_mode-image_xfm.mat` (`<a>` = the raw file's `acq`, else the bundle key's orientation: `ax` / `cor` / `sag`) |
| normalize | T2w in anchor space (second channel) | `coregistration/anat/<raw T2w entities>_space-anchorNative_desc-preproc_T2w.nii.gz` + `coregistration/transformations/sub-<id>_ses-preop[_acq-<a>]_from-T2w_to-anchorNative_mode-image_xfm.mat` (`<a>` = the raw T2w's `acq`) |
| normalize | anchor in template space | `normalization/anat/<raw anchor entities>_space-<Space>_desc-preproc_<mod>.nii.gz` |
| normalize | post-op images in template space | `normalization/anat/<raw entities>_space-<Space>_desc-preproc_<suffix>.nii.gz` (CT and every MR key) |
| normalize | warps | `normalization/transformations/sub-<id>_from-anchorNative_to-<Space>_mode-image_xfm.h5` (forward composite) and `sub-<id>_from-<Space>_to-anchorNative_mode-image_xfm.h5` (inverse) |
| normalize | linear part | `normalization/transformations/sub-<id>_from-anchorNative_to-<Space>_desc-affine_mode-image_xfm.mat` (ITK binary affine = the rigid + affine element of the forward composite; ITK convention: maps template points to native points) and `sub-<id>_from-<Space>_to-anchorNative_desc-affine_mode-image_xfm.mat` (its inverse); written by both engines in the same staged set as the composites; an approved lock without them gets them back-filled from the approved forward composite on the next run (note logged, method log untouched). The method log lists `transforms: {forward, inverse, forward_affine, inverse_affine}` and `affine_native_to_template_4x4` (RAS mm, row-major, native → template) |
| normalize | registration grid scratch | `normalization/work/`, removed at stage end; the normalize checkreg gate writes `template_is<mm>mm.nii.gz` there again and removes the folder once the review approves (`04` §5.4 step 3) |
| coreg / normalize / brainshift | method logs (`03` §3, `04` §5.2 step 6, QC gate `03` §12.4a) | `coregistration/log/sub-<id>_desc-coregmethod.json`, `normalization/log/sub-<id>_desc-normmethod.json`, `brainshift/log/sub-<id>_desc-brainshiftmethod.json` |
| registration stages | checkreg image | `<stage folder>/checkreg/<image stem>.png` (`<image stem>` = the checked image's name without `.nii.gz`) |
| brainshift | images and transform (not implemented) | reserved: `brainshift/anat/<raw CT entities>_space-anchorNative_desc-brainshift_CT.nii.gz`, `brainshift/transformations/sub-<id>_ses-postop_from-anchorNative_to-anchorNative_desc-brainshift_mode-image_xfm.mat`. No helper in `derivatives.rs` produces them and no stage writes them; the only brainshift body (`DBSLOC_SCRF_ANTS=1`, `00` §5.2) takes its paths from `DBSLOC_SCRF_MOVING` / `_REFERENCE` / `_OUTPUT` / `_WARP` / `_AFFINE` |
| reconstruct | reconstruction record (primary) | `reconstruction/sub-<id>_desc-reconstruction.json` (`05` §8) |
| reconstruct | Lead-DBS compatible twin | `reconstruction/sub-<id>_desc-reconstruction.mat` (MATLAB v7 Level-5, variables `reco` + `dbsloc_provenance`, `specs/12` Part 1), rewritten whenever the JSON is written (reconstruct, orientation, manual correction) |
| reconstruct | external PaCER result (input, read only) | `reconstruction/sub-<id>_desc-pacer_import.json` (`05` §3.4) |
| reconstruct | PaCER brain masks (`02` §7) | `reconstruction/<raw CT entities>_space-CT_desc-brain_mask.nii.gz`, `reconstruction/<raw CT entities>_space-anchorNative_desc-brain_mask.nii.gz` |
| visualize | scene manifest | `reconstruction/viz/scene_manifest.json` |

**Other subject-level outputs** (named by their own modules; `<label>` = stimulation label, `<slug>` = Lead-DBS model slug of `07` §1.1, `<H>` = `R` or `L`):

| Owner | Output | Name |
| --- | --- | --- |
| `07` §2.3, `vat/settings.rs` | canonical stimulation settings | `stimulations/native/<label>/sub-<id>_desc-stimulation.json` |
| `07` §4–§5, `vat/outputs.rs` | Lead-DBS settings twin | `stimulations/<space>/<label>/sub-<id>_desc-stimparameters.mat` (MAT v7) in every space folder of the label; `<space>` = `native`, `<Space>`, and `MNI152NLin2009bAsym` when it differs from `<Space>` |
| `07` §5.1, `vat/outputs.rs` | VTA images | `stimulations/<space>/<label>/sub-<id>_sim-binary_model-<slug>_hemi-<H>.nii`; for model `efield` also `…_sim-efield_…` and `…_sim-efieldgauss_…` |
| `07` §5.2, `vat/outputs.rs` | VTA mesh and volume | `stimulations/<space>/<label>/sub-<id>_sim-binary_model-<slug>_hemi-<H>.mat` (MAT v7) |
| `07` §8.4, `vat/outputs.rs` | results sidecar, atlas statistics | `stimulations/<space>/<label>/sub-<id>_desc-vta.json` (every space folder); `sub-<id>_desc-vtastats.json` (template and reference-space folders) |
| `07` §8.4, `14` §3 | Lead-DBS statistics | `sub-<id>_desc-stats.mat` at the subject folder root (`ea_stats`, MAT v7); backup `sub-<id>_desc-stats_backup.mat` (`14` §3.4) |
| `07` §3.6, `vat/basis_cache.rs` | E-field basis cache | `headmodel/native/sub-<id>_hemi-<H>_desc-efieldbasis.{json,bin}` (internal format; Lead-DBS's own files in that folder are left alone) |
| `14` §4.4, `group/flip.rs` | mirrored stimulation images | `stimulations/<Space>/gs_<guid>/…_hemi-R_hemidesc-FlippedFromLeft.nii` and `…_hemi-L_hemidesc-FlippedFromRight.nii` beside the originals |
| `15` §9.2, `connectome_maps/job.rs` | seeds, maps, per-fibre values | in `stimulations/<Space>/<label>/`: `sub-<id>_sim-<kind>_model-<slug>_seed-dMRI.nii` and `…_seed-fMRI.nii` (each also per `_hemi-L` / `_hemi-R`), `…_seed-dMRI_conn-<tag>_strucmap.nii`, `…_seed-fMRI_conn-<label>_desc-{AvgR,VarR,AvgRFz,T}_funcmap.nii`, optional `…_seed-fMRI_conn-<label>_id-<subject id>_corr.nii`, `sub-<id>_sim-efield_model-<slug>_hemi-<H>_conn-<tag>_fibervals.mat`, `…_seed-dMRI_conn-<tag>_fibers.mat`; each with a provenance sidecar `<name>.json` (`15` §9.3) |
| `16` §5, `reports.rs` | session reports | `clinical/reports/index.json`, `clinical/reports/<vendor>/<folder>/sub-<id>_desc-sessionreport.json` |

**Stimulation images are `.nii`.** Every NIfTI under `stimulations/` is uncompressed `.nii`, not `.nii.gz`, because Lead-DBS matches these names with a `.nii` end anchor (`07` §5.1, `14` D12, `15` D7); readers also accept `.nii.gz` in imported Lead-DBS folders. A label folder may hold Lead-DBS outputs of other models, which are left alone (`07` §10). Labels starting with `gs_` are reserved for group stimulations (`07` §2.6 rule 10). Lead Group files live in the group folder `<dataset>/derivatives/leadgroup/<guid>/` (`14` §2.3, §11), which does not move with the output root.

**Method logs are written by the stage**, not only by the QC viewer: every completed `coreg`, `normalize` and `brainshift` writes its method log through the staged write with `approval: 0` (a fresh result needs fresh QC) and the keys `method`, `engine` (`ants` or `wgpu`), `custom: false`, `version` (the `dbsloc` version) plus the stage's details (`03` §3, `04` §5.2 step 6). The approved-lock checks of `00` §6 read the method log.

---

## 8. Raw images and import

### 8.1 Raw image rules

The import enforces these rules on the images it writes (§8.4.1). The resolver is more lenient and behaves as §10 states (for example, it accepts any pre-op suffix from an index row as anchor).

1. A subject has at least one pre-op image with a recognised suffix (§4); an import that would leave it without one fails with `E_NO_ANCHOR`.
2. Post-op images are optional; a subject with only pre-op images resolves with post-op modality none.
3. A post-op image is either a `CT`, which carries no `acq`, or an MR with the orientation `acq-ax`, `acq-cor` or `acq-sag`. Any one orientation suffices: a subject whose only post-op MR is coronal or sagittal is a post-op MRI subject (the reference application requires an axial MR, `13` §4.7).
4. At most one image per (session, suffix, `acq`).

**Orientation guess (normative).** The import proposes an `acq` for an image (`import.rs` `fn acq_from_name_or_voxels`, `fn guess_acq`):

1. When the lower-cased file name contains `acq-ax`, `acq-cor`, `acq-sag` or `acq-iso` (tested in this order), that orientation is used.
2. Otherwise the voxel sizes are the absolute values of `pixdim[1..3]`. When the largest is not positive, or the largest and smallest differ by at most 10 % of the largest, there is no guess. Otherwise the slice axis is the voxel axis whose size is farthest from the median of the three sizes (on a tie, the larger size, then the first axis). For the usual square in-plane pixels this is the axis normal to the plane of the two equal sizes, whether the slices are thicker or thinner than the pixels: `(3, 3, 0.5)` and `(1, 1, 3)` both give the third axis.
3. The slice axis direction is that column of the sform 3 × 3 part when `sform_code > 0` and the part is not all zero, else of the qform quaternion rotation. It maps to the world axis with the largest absolute component (the first of x, y, z on a tie): x → `sag`, y → `cor`, z → `ax`.
4. Without a guess the image gets no `acq`; `acq-iso` comes only from the file name or the user. A post-op MR without `ax`, `cor` or `sag` gets `ax`.

### 8.2 Sources of raw images

The resolver accepts three kinds of subject, which may be mixed (§9.3):

- **Imported** (§8.4): files `ses-<session>/anat/sub-<id>_ses-<session>[_acq-<acq>]_<suffix>.nii.gz`, listed in `rawdata/sub-<id>/rawimages.json` (§9.1).
- **Prepared by Lead-DBS:** files in the same folders, listed in the Lead-DBS index `prefs/sub-<id>_desc-rawimages.json` of the subject's output folder (§9.2).
- **Placed by hand:** BIDS-named files without an index, classified by file name (§10 steps 3–4).

### 8.3 Legacy patient folders (non-goal)

Flat pre-BIDS Lead-DBS patient folders are not read and not migrated. The reference migration is recorded in `13` §2.8.

### 8.4 Import of NIfTI files and DICOM folders (normative, 2026-10-14; amended 2026-10-22 and 2026-10-25)

Users have loose files, not datasets. One module, `dbsloc_pipeline::import`, turns them into the layout of §2–§4. The GUI wizard, `dbsloc import` and the server only collect the inputs and call it (no import logic in a front end, `AGENTS.md` §3).

#### 8.4.1 Items and plan rules

`fn plan_import_with(root, output_dir, subject, items, sourcedata_dirs)` validates an import and computes its destinations, index rows, archives and warnings; it writes nothing. `output_dir` is the output root that holds the Lead-DBS index and the locks a replacing import resets. An item (`ImportItem`) is a source NIfTI (`.nii` or `.nii.gz`), an optional JSON sidecar, a session (`preop` | `postop`), a suffix and an optional `acq` (`ax`, `cor`, `sag`, `iso`).

Destination of an item: `rawdata/sub-<id>/ses-<session>/anat/sub-<id>_ses-<session>[_acq-<acq>]_<suffix>.nii.gz`; its sidecar, when given, goes beside it as `….json`. Its index row (§9.1): `{"path": "<destination relative to the dataset root>", "session": "<session>", "datatype": "anat", "entities": {"suffix": "<suffix>"[, "acq": "<acq>"][, "modality": "CT" | "<acq>_MRI"]}}`, with `modality` on post-op rows only.

Rules; the first failure ends the plan:

1. An invalid subject id → `E_BIDS_PARSE` (`bids::parse_subj_id`, §3). No items → `E_IMPORT_RULES` `no images to import`. An existing `rawimages.json` that does not parse → `E_IMPORT_FILE`.
2. Every source must be a usable single-file NIfTI-1, else `E_IMPORT_FILE` `<path>: <reason>`: the name ends `.nii` or `.nii.gz` (`not a NIfTI file (.nii or .nii.gz)`); `sizeof_hdr` is 348 in either byte order and the magic is `n+1` (a NIfTI-2 header: `NIfTI-2 is not supported`); `dim[0]` is 3 to 7, `dim[1..3]` are positive, the datatype is not 0, `bitpix` is positive and `qform_code` or `sform_code` is positive (each failure: `unusable NIfTI header: …`). A `.nii` must hold at least `max(vox_offset, 352) + ⌈∏dim · bitpix / 8⌉` bytes (`image data is truncated: <n> bytes, header needs <m>`); a `.nii.gz` is checked while it is copied (§8.4.2).
3. A given sidecar must be a file (`E_IMPORT_FILE` `<path>: sidecar is not a file`).
4. The suffix must be allowed for the session: pre-op `T1w`, `T2w`, `PDw`, `T2starw`, `FLAIR`, `FGATIR`; post-op `CT` or one of those. Else `E_IMPORT_RULES` `<path>: suffix "<suffix>" is not allowed for Pre-op images (choose one of …)` (`Post-op` for the other session).
5. A `CT` with `acq` → `E_IMPORT_RULES` `<path>: CT images carry no orientation`.
6. A post-op MR without `ax`, `cor` or `sag` → `E_IMPORT_RULES` `<path>: a post-op MR image needs an orientation (axial, coronal or sagittal)`. Pre-op items may carry any `acq`.
7. Two items with equal (session, suffix, `acq`) → `E_IMPORT_RULES` `duplicate image: <Session> <suffix>[ (<orientation>)] appears more than once` (the resolver would raise `E_DUPL_RAW`).
8. A destination path (absolute) longer than 240 characters is a warning: `the destination path of <name> is longer than 240 characters and may fail on Windows` (staging names add `.partial-`).
9. Without a pre-op item, the subject folder must exist and resolve without `E_NO_ANCHOR`; else `E_NO_ANCHOR` `no pre-op image: sub-<id> would have no pre-op anchor, add a pre-op anatomical image (e.g. T1w)`.
10. The merged index (the existing `rawimages.json` rows that no new row replaces, the new rows, then the Lead-DBS index rows resolved as if the planned files existed) must not hold two rows with equal (session, datatype, entities); else `E_DUPL_RAW` `<duplicate rows>: after the import the subject would not resolve, rows of <rawimages.json> or of the Lead-DBS index <prefs index> share a session, type and orientation; remove the duplicate row or leave the image out`.
11. **Conflicts.** A planned file whose destination image or sidecar exists, or whose destination path an existing index row names, is a conflict (`ImportPlan::conflicts`). An existing row that a new row replaces (same key or same path) also gives the warning `the existing index row for <path> will be replaced`.
12. Every folder in `sourcedata_dirs` must be a folder (`E_IMPORT_FILE` `<path>: DICOM folder to keep is not a folder`); its archive destination is planned as in §8.4.2.

#### 8.4.2 Writing, outputs and rollback

`fn run_import(plan, replace_existing, cancel, progress)`:

- Without `replace_existing`, any conflict (checked again on disk) fails with `E_IMPORT_RULES` `files already exist: <destinations>` before anything is written.
- Order: the dataset root folder, `dataset_description.json`, the DICOM archives, then per item the image and its sidecar, then `rawimages.json`. Progress reports `done / total` and the current name.
- Every file is written as `.partial-<name>` and renamed into place; a file being replaced is first renamed to `.partial-backup-<name>`. A journal records created files, replaced files, created folders, archive folders and the previous index.
- **Images** are always `.nii.gz`. A `.nii.gz` source (also dcm2niix output) is copied byte for byte while it is fully decoded, never re-compressed, so a truncated or corrupt stream fails; a `.nii` source is compressed once into a single gzip member by the parallel writer at level 3. The decoded size must reach the length the header needs (§8.4.1 rule 2).
- **`rawimages.json`** (§9.1): the existing rows are kept except those with the same (session, datatype, entities) or the same path (compared with `\` turned into `/` and leading `./` removed) as a new row; the new rows follow. The file is written pretty-printed through `.partial-`.
- After writing, the subject must resolve (§10); a failure rolls the import back.
- **`dataset_description.json`**: when missing, `{"Name": "<root folder name>", "BIDSVersion": "1.9.0", "DatasetType": "raw", "GeneratedBy": [{"Name": "Wyss DBS Localizer", "Version": "<version>"}]}`; an existing JSON object without `GeneratedBy` gets that key appended (key order kept); an unreadable or non-object file is left alone.
- **Source DICOM archive** (`SubjectImport.keep_sourcedata`, default on; the plan's `sourcedata_dirs`): each DICOM input folder is copied to `sourcedata/sub-<id>/DICOM/<name>` through `.partial-<name>`. `<name>` is the folder name reduced to ASCII letters, digits, `.`, `-` and `_` (at most 128 characters, leading dots removed; `dicom` when empty); when it is taken, `-2`, `-3`, … is appended with the warning `<path> exists already, the DICOM folder is kept as <path>-<n>`. A folder already under the subject's `sourcedata` is skipped with the warning `<folder> is already under <path>, it is not copied again`. Only regular files are copied; symbolic links are not followed and folders deeper than 32 levels are not copied.
- **Rollback.** On any error or cancel (`E_IMPORT_CANCELLED` `import cancelled, nothing was kept`) the previous index is restored (or the new one removed), created files and folders are removed, replaced files are restored and archive folders are removed. Failures during the rollback are appended to the message as ` (rollback incomplete: …)`. On success the backups are deleted.

#### 8.4.3 Invalidation by a replacing import

An import replaces a session when an image destination already existed as a file or an existing index row of that session was replaced. The stages that read the replaced images are then invalidated as in `00` §6.1 (lock `approval: false`, method-log approval cleared, `invalidated_by: "import"`; `invalidate::invalidate_from`): a replaced **pre-op** image → `bids`, `preprocess` and their transitive dependents; otherwise a replaced **post-op** image → `coreg` and its transitive dependents. A replaced sidecar alone and an import that only adds files invalidate nothing. The import report (`ImportReport.invalidation`) carries the result; the log line is `dbsloc: import replaced raw images; approvals invalidated: <stage>, …` (followed by `; reviews reset: <stage>, …` when reviews were reset), shown in the GUI log and by `dbsloc import`; the server returns the invalidated stages in its commit response.

#### 8.4.4 DICOM conversion

- **Converter.** dcm2niix v1.0.20260724 (BSD-2-Clause, `NOTICE`), bundled under `resources/dcm2niix/{linux,mac,win}/`. `fn find_dcm2niix`: `DBSLOC_DCM2NIIX_BIN` (when it names a file) → the bundled copy → `PATH`. None → `E_DCM2NIIX` `dcm2niix was not found: set DBSLOC_DCM2NIIX_BIN or install dcm2niix on PATH`.
- **DICOM folder** (`fn looks_like_dicom`): a regular file up to 32 levels below it has the extension `.dcm` or `.ima` (any case) or the bytes `DICM` at offset 128.
- **Invocation** (`DCM2NIIX_ARGS`): `dcm2niix -z y -b y -i y -f %f_%p_%z_%t_%s_%d -o <empty temporary folder> <folder>`: gzip output, BIDS sidecars, derived, localizer and 2-D images skipped, file names from the input folder, protocol, sequence, date-time, series number and description. Standard input is closed; no network. Cancelling kills the process (`E_IMPORT_CANCELLED` `DICOM conversion cancelled`).
- **Failures** (`E_DCM2NIIX`): output folder not empty (`output folder <dir> is not empty`), the converter cannot start, a non-zero exit (`dcm2niix failed (<status>):` followed by the last 20 non-empty output lines), no NIfTI produced (`no NIfTI series produced`). On failure or cancel the output folder is emptied.
- **Converted series** (`ConvertedSeries`): every NIfTI in the output folder, with its same-stem sidecar when present, its metadata and a suggestion (§8.4.5); sorted by series number (series without one last), then path.
- **Privacy.** Sidecars come from dcm2niix's default BIDS anonymisation (`-ba y` is its default with `-b y`: no patient name, id, birth date or other identifying tags), so copying them beside the image adds acquisition parameters, not identity. The server additionally blanks the NIfTI `descrip` and `aux_file` fields of uploads.

#### 8.4.5 Series metadata and suggestions

- **Metadata** (`fn series_info`, `SeriesInfo`; from the sidecar, else the file name; missing values allowed): series number, description, protocol, DICOM modality, `acquisition_datetime`, `study_date`, image type, manufacturer, field strength, TE, TR, TI, slice thickness, dimensions and voxel size (zeros when the header fails validation). `acquisition_datetime` = `AcquisitionDateTime` (its time part, else `AcquisitionTime`), else the first valid date of `AcquisitionDate`, `SeriesDate`, `ContentDate`, `StudyDate` with `AcquisitionTime` or the matching time, else a 14-digit `YYYYMMDDHHMMSS` token in the file name; years 1900–2200. `study_date` = `StudyDate`, else the acquisition date.
- **Lookup table** (`LookupTable`; default `fn default_lookup_table`, replaced by `DBSLOC_IMPORT_LOOKUP_JSON` or a table the GUI supplies). JSON form: `[{"session": "preop" | "postop", "suffix": "<suffix allowed for the session>", "keywords": ["…"]}]`, unknown fields refused, suffix matched case-insensitively. Default rules, in order: pre-op `T1w` ← `t1`, `mprage`; `T2w` ← `t2`, `tse`; `FGATIR` ← `fgatir`; `PDw` ← `pd`; `T2starw` ← `t2star`, `t2*`, `swi`; `FLAIR` ← `flair`; post-op `CT` ← `ct`. An invalid table file stops `dbsloc import` (exit 2); the GUI falls back to the default rules with a warning.
- **Matching** (`fn suggest`): the text searched is SeriesDescription, ProtocolName and the file stem, lower-cased; for a converted series the input folder name is removed from the start of the stem (dcm2niix's `%f` puts it there). A keyword of at most three ASCII letters or digits matches a whole word (words split at characters that are not letters or digits), optionally followed by `w` (`t1` matches `T1w`, `ct` does not match `direct`); a longer keyword matches anywhere as a plain substring. **The last matching rule wins.**
- **DICOM modality.** A sidecar `Modality` of `CT` (any case) always means post-op `CT`; any other non-empty value disables the `CT` rules.
- **Result.** A `CT` rule gives post-op `CT` without `acq`; a pre-op rule gives that suffix with the orientation guess of §8.1; a post-op MR rule gives that suffix with the guess, `ax` when there is none.
- **Unassigned.** When no rule matches, the series is kept with `assigned = false` and a placeholder pre-op `T1w` with the guessed `acq`; it is not imported unless the user assigns it.
- **Study-date rule** (opt-in, `fn apply_latest_study_rule`): series are grouped into studies by `study_date` (oldest first, undated last; `fn group_studies`); with two or more dated studies, every assigned pre-op non-CT series of the latest dated study becomes post-op MR (`acq` kept when `ax`, `cor` or `sag`, else `ax`).
- **Subject id from a folder** (`fn subject_id_from_folder`): a leading `sub-` is removed and every character other than `A–Z`, `a–z`, `0–9` is dropped, with the warning `subject id "<id>" from folder "<name>": characters other than A-Z, a-z, 0-9 were removed`. The folder of a NIfTI input is its parent; a folder named `DICOM` (any case) gives its parent's name.

#### 8.4.6 Batch import

`fn plan_import_batch` / `fn run_import_batch` import several subjects into one dataset, one after another, each with its own plan and journal. A subject id that occurs twice fails for its second occurrence (`E_IMPORT_RULES` `subject sub-<id> appears more than once in the batch`). A failed subject is rolled back and the next one runs; a cancel rolls back the current subject and reports each remaining one as `E_IMPORT_CANCELLED` `import cancelled before sub-<id>`. One result per subject, in order.

#### 8.4.7 Front ends

**GUI wizard** (`crates/dbsloc-app/src/import_wizard.rs`, `docs/agent-rules/gui-ux.md`; header *Import… → Images into a subject…*, `Ctrl+I`, the start screen, the guided Subject step). Three steps with Back / Next / Import in a pinned bar:

1. *Images* — *Add folders…* (several; a folder with NIfTI files at its top level is added as it is, any other folder is converted as DICOM on a cancellable worker, one folder at a time, with the active lookup table), *Add NIfTI files…* (a same-stem JSON beside a file is its sidecar), *Post-op = latest study* (§8.4.5). An input list (with the subject id each folder gives) and a series table grouped by study (oldest first, undated last), one row per image with a tick, its metadata, session, type and orientation (post-op MR: ax / cor / sag, axial by default; CT: none) and a *View* button (single-volume preview, `08_addendum_interactive_volume_viewer.md` §6.7). Unassigned rows start unticked and greyed; editing a row ticks it. Rule errors from a dry-run plan show live and block *Next* (`E_NO_ANCHOR` is ignored on this step).
2. *Subject and destination* — subject id prefilled from the first input folder; dataset folder (default: the open or last-used dataset); the dataset's output folder, read-only; *Keep a copy of the source DICOM in sourcedata* (on; shown when a DICOM input has ticked rows); *Replace existing images* (when the subject exists); and the post-op modality to use (Automatic / CT / MRI; shown only when a post-op CT and a post-op MR are both ticked; stored as the GUI preference that applies to subjects with both modalities, §10 step 5).
3. *Review* — the dry-run plan (planned files, replacements, `sourcedata` copies, warnings, the number of unticked images left out). `E_NO_ANCHOR` blocks; conflicts block until *Replace existing images* is ticked; when no post-op image is ticked and none is on disk the wizard asks `No post-op image is assigned for <subject>. Import anyway?`. The import runs with progress; success opens the subject, failure shows the error window.

**Several subjects:** when the input folders give different ids, *One subject per input folder* (on by default) imports them with `run_import_batch` (inputs with the same id merged, subjects without ticked rows dropped, subjects whose plan failed skipped and reported), with per-subject progress and a result list. Screenshot-mode variables: `docs/RUN_REFERENCE.md`.

**Command line:** `dbsloc import <root> [--subject ID] [--output-dir DIR] [--dicom FOLDER]… [--nifti FILE]… [--item SESSION:SUFFIX[:ACQ]=SOURCE]… [--auto] [--postop-latest-study] [--no-keep-sourcedata] [--replace-existing] [--dry-run] [--json]` imports one subject; flags, output and exit codes are in `docs/RUN_REFERENCE.md`. Unassigned images stop it (exit 2, listed) unless `--auto` leaves them out.

**Server:** the upload API converts with `fn convert_dicom` and commits with `fn plan_import` / `fn run_import` after its header scrub (`docs/SERVER_CLIENT_PLAN.md`).

#### 8.4.8 Import and runs are exclusive in the GUI (normative, 2026-11-12)

An import replaces raw images, rewrites `rawimages.json` and invalidates locks (§8.4.3); a job reading the same subject meanwhile would certify results computed from the old images. The GUI therefore never runs an import together with a job that writes stage results, approvals or locks: a run or a run of several subjects (`specs/13` §9), a registration review (prepared, open, or its clean-up running), *Refine localization* (opening, open or saving) and the group *Compute VTAs* job (until its worker has ended). The wizard does not open while one is active; while it is open, *Import* is disabled and a late request is refused with every choice kept; while the import writes, none of those jobs starts (a single run is refused with `Wait until the image import has finished writing into the dataset.`, a batch waits). DICOM conversion into the temporary folder holds back nothing.

Left out of this lock, because they write neither raw images, `rawimages.json` nor stage outputs, approvals or locks: stimulation *Save* (`07` §2.3, §7.2), stimulation *Compute connectivity* and the group *Analysis* jobs (provenance sidecars mark their outputs stale, `15` §8, §9.3; `07` §7.6), and the session-report import (`16` §5.1).

Entry points, disabled controls, hover texts and refusal windows: `specs/08` §11.1.9.

---

## 9. Raw image indexes

Two index files can list a subject's raw images. Neither is regenerated from the folders by this program.

### 9.1 `rawdata/sub-<id>/rawimages.json`

The program's own index, written only by the import (§8.4.2), which merges rows into it. A JSON array of rows `{"path", "session", "datatype", "entities"}` (`bids::load_rawimages`); a missing or non-string field reads as empty.

- `path`: relative to the dataset root (the import writes `/` separators); an absolute path also works.
- `session`: `preop` or `postop`; `datatype`: `anat`. Rows with another session or datatype, or an empty path, are ignored.
- `entities`: `suffix` (needed for a pre-op row to count), optional `acq`, and on post-op rows `modality` = `CT`, `ax_MRI`, `cor_MRI` or `sag_MRI`. A post-op row without a usable `modality` is classified by its `suffix`, `acq` and file name (§10 step 4).

### 9.2 Lead-DBS index `<output root>/sub-<id>/prefs/sub-<id>_desc-rawimages.json`

Written by Lead-DBS, read and never written here (`bids::load_leaddbs_prefs_index`). Shape `{"preop" | "postop": {"anat": {"<logical key>": "<file stem>"}}}` (Lead-DBS keys such as `iso_T1w`, `CT`, `ax_MRI`). A stem resolves to `rawdata/sub-<id>/ses-<session>/anat/<stem>.nii.gz`, else `<stem>.nii`, else the first NIfTI in that folder (ordering rule, §10) whose name starts with the stem; an unresolved or empty stem is skipped. Each entry becomes a row with `entities = {"suffix": <suffix of the logical key>, "modality": <logical key>}`. Logical key → suffix (`bids::logical_key_to_bids_suffix`): `CT` or a key ending `_CT` → `CT`; else the first of `T1w`, `T2w`, `T2starw`, `PDw`, `FLAIR`, `FGATIR`, `T1map`, `T2map` that the key equals, ends with or contains as `_<suffix>`; else `T1w` when the key contains `T1`, `T2w` when it contains `T2`; else the key itself.

### 9.3 Merging and duplicates

Unless indexes are disabled (`--no-index`, `RunOptions.use_index = false`), the rows of §9.1 come first, then those of §9.2. Two rows with equal (session, datatype, entities), entities compared with sorted keys, fail with `E_DUPL_RAW`, whether or not their files exist. Malformed JSON in either file fails with `E_BIDS_PARSE`. Rows whose file does not exist are then skipped; within each session the first row per pre-op suffix or post-op bundle key wins. Without index rows the folders are scanned (§10 steps 3–4); pre-op files are scanned only when the indexes give no pre-op row, post-op files are always scanned.

---

## 10. Resolving a subject

### 10.1 Algorithm

`bids::resolve_subject_for(dataset, output_dir, subject, use_index, postop_choice)`; `resolve_subject_in` and `resolve_subject_with` call it with `PostopModality::Auto` (and the default output root for the latter); `RunOptions::resolve_subject` passes the run's options. A run resolves the subject at its start (the `bids` stage has no body); a failure ends the run (`00` §5.2).

1. **Id and folders.** Parse the id (§3); canonicalize the dataset root and derive the output root (§2). `rawdata/sub-<id>/` must be a directory, else `E_NO_SUBJ`.
2. **Indexes** (§9), unless disabled.
3. **Pre-op.** Pre-op modalities come from the index rows. When there are none, every NIfTI below `ses-preop/anat/` whose name ends `_<suffix>.nii` or `_<suffix>.nii.gz` for a suffix of §4 (case-sensitive) is taken, the first path per suffix by the ordering rule. Anchor modality per §4; no pre-op modality at all → `E_NO_ANCHOR`.
4. **Post-op bundles.** Index rows first, then the NIfTI files below `ses-postop/anat/` in the ordering rule, each keyed by the table below; the first path per key wins, and a file that an index row already placed is not added again under a second key. The bundles keep this insertion order; it is not re-sorted, and `postop_raw` iterates in it.
5. **Post-op modality.** **CT present** = the bundles have a `CT` key. **MR present** = a NIfTI below `ses-postop/anat/` has a lower-cased name containing `acq-ax`, `acq-cor` or `acq-sag`, or an index row (either index) produced an `ax_MRI`, `cor_MRI` or `sag_MRI` bundle. Any one orientation suffices. A post-op file without an `acq` orientation and without an index row still enters the `ax_MRI` bundle (step 4) but does not count as MR present. `both_mrct_present` = CT present and MR present. **Choice** (`bids::PostopModality`, owner decision 2026-10-22):
   - `Auto` (default): `CT` when only a CT is present, `MRI` when only an MR is present, none when neither is. When both are present: `MRI` if the subject's `prefs/sub-<id>_desc-uiprefs.mat` stores `modality = 1`, else `CT` (a stored `2`, any other value, a missing or unreadable file). Lead-DBS keeps this choice in `uiprefs.mat`; it is read here, never written.
   - `Ct` / `Mri` (`RunOptions.postop_modality`, `DBSLOC_POSTOP_MODALITY` = `auto` | `ct` | `mri`): force one; fail with `E_MISS_POSTOP` when that modality is not present.
   - The GUI passes its preference only for a subject that has both modalities, `Auto` otherwise.
   Reconstruction takes the CT path only for `CT`; for `MRI` it reconstructs in template space from `Subject::norm_postop_mr_in_use()` (`05` §3).
6. **Derived files.** Fill `preop_anchor_preproc` and the bundles of §6.4–§6.5 from the §7a names, for files that exist.

**Post-op bundle key (step 4).** Name tests use the lower-cased file name. An index row's logical key is its `modality` entity; when that is missing, null, not a string or blank (hand-written indexes), it is `CT` when the row's `suffix` is `CT`, else `<acq>_MRI` when its `acq` is `ax`, `cor` or `sag` (both compared trimmed and case-insensitively), else none. A post-op row never defaults to `CT`.

| Scanned file (first test that holds) | Index row (first test that holds) |
| --- | --- |
| name ends `_ct.nii` or `_ct.nii.gz` → `CT` | logical key `CT` (any case), or name contains `_ct.nii` → `CT` |
| name contains `acq-cor` → `cor_MRI` | name contains `acq-cor` or `_cor_` → `cor_MRI` |
| name contains `acq-sag` → `sag_MRI` | name contains `acq-sag` or `_sag_` → `sag_MRI` |
| — | name contains `acq-ax` → `ax_MRI` |
| — | logical key `cor_MRI` / `sag_MRI` (any case) → that key |
| otherwise `ax_MRI` | otherwise `ax_MRI` |

**Ordering rule.** Wherever the resolver picks one file among several (pre-op file per suffix, post-op file per bundle key, Lead-DBS stem match) it sorts by the key (lower-cased path, original path) and takes the first; the result is the same on case-sensitive and case-insensitive file systems and with any directory-listing order. Folder scans do not descend into symbolic links or junctions to directories and skip `.partial-*` files (in-progress outputs, `00` §5.2).

**Idempotent:** resolving again gives the same record while the disk is unchanged.

### 10.2 Not part of resolution

Resolution loads no template; the template comes from `04` §3.1. `dbsloc subject show <dataset> <subject> [--no-index] [--output-dir DIR]` prints `subj_id`, `subj_dir`, `anchor_modality`, `postop_modality` (`Auto` choice), `preop_raw` and `postop_raw`, and, when `<dataset>/spacedef.json` exists, `space_name` and `primary_template` (`crates/dbsloc-cli/src/spacedef.rs`: the anchor key with a leading `iso_`, `ax_` or `r_` removed, looked up in `norm_mapping`, else the first mapping key the anchor ends with or contains, else `misfit_template`, default `t1`). A malformed `spacedef.json` exits 1.

---

## 11. Errors

| Code | Raised by | When | Message |
| --- | --- | --- | --- |
| `E_BIDS_PARSE` | resolver, import | invalid subject id; malformed index JSON | `not a valid sub-<id> folder name: "<name>"`; `<index path>: <reason>` with reason a JSON or read error, `rawimages root must be a list`, `rawimages row must be an object` or `Lead-DBS rawimages top level must be an object` |
| `E_NO_SUBJ` | resolver | `rawdata/sub-<id>/` missing | `expected <rawdata/sub-<id>> (BIDS: rawdata/sub-<id>/). Create it under the dataset root <root> or pass the correct --dataset-root. (E_NO_SUBJ)` |
| `E_NO_ANCHOR` | resolver; import plan | resolver: no usable pre-op index row and no scanned pre-op file with a recognised suffix (§10 step 3); import: §8.4.1 rule 9 | resolver: `no pre-op anchor: add a NIfTI under <ses-preop/anat> (e.g. *T1w.nii.gz) or list files in <rawimages.json> / prefs index. (E_NO_ANCHOR)`; import: §8.4.1 rule 9 |
| `E_DUPL_RAW` | resolver; import plan | duplicate index rows (§9.3) | `duplicate index rows for sub-<id>: [("<session>", "<datatype>", {<entities>}), …] (E_DUPL_RAW)`; import: §8.4.1 rule 10 |
| `E_MISS_POSTOP` | resolver | the forced post-op modality is not present (§10 step 5) | `post-op modality <ct|mri> was requested but sub-<id> has no post-op <CT|MR> image (found: <CT|MR|none>). (E_MISS_POSTOP)` |
| `E_IMPORT_FILE` | import | a source missing, not a NIfTI, or unusable; a sidecar not a file; an unparsable `rawimages.json`; a write failure | `<path>: <reason>` (§8.4.1–§8.4.2) |
| `E_IMPORT_RULES` | import | items break §8.1 / §8.4.1; conflicts without replace; a duplicate subject in a batch | §8.4.1, §8.4.2, §8.4.6 |
| `E_DCM2NIIX` | import | converter not found, failed, or produced no series | §8.4.4 |
| `E_IMPORT_CANCELLED` | import | the user cancelled an import or conversion; everything written is rolled back | `import cancelled, nothing was kept`, `DICOM conversion cancelled`, `import cancelled before sub-<id>` |

An import error displays as `<message> (<code>)`. Command exit codes: `docs/RUN_REFERENCE.md`.

Implementation status: in `bids.rs` `fn resolve_subject_for` the `E_MISS_POSTOP` message holds a run of spaces before `(found: …)` (a broken string continuation); the intended text has one space.
Implementation status: the resolver messages already end in their code (`(E_NO_SUBJ)`, `(E_NO_ANCHOR)`, `(E_DUPL_RAW)`, `(E_MISS_POSTOP)`); when the import passes one on (`From<BidsError> for ImportError`, also inside the plan-time `E_DUPL_RAW` message), its display appends the code a second time.
Implementation status: the `E_NO_SUBJ` message names a `--dataset-root` flag that no command has (the dataset root is a positional argument).

---

## 12. Acceptance

The unit tests of `bids.rs`, `derivatives.rs`, `import.rs` and `import_wizard.rs` and `crates/dbsloc-cli/tests/cli_import.rs` / `cli_subject.rs` check:

- **Resolution:** id forms and error codes; `T1w` anchor over `T2w`; anchor in use precoreg > N4 > raw; both indexes, Lead-DBS logical keys, duplicate rows; post-op rows without `modality` never become `CT`; the post-op choice (CT + MR → `CT` unless `uiprefs.mat` stores MR, forced choices, coronal-only or sagittal-only MR); case- and order-independent file choice, insertion-ordered bundles, `.partial-*` ignored, a chosen output root.
- **Names:** every row of the §7a stage-output table, the image and transform rules, entity round trip.
- **Import:** each plan rule and its code; conflicts, replace and rollback; truncated or corrupt sources leave nothing; cancel rolls back; `.nii.gz` copied unchanged, `.nii` written as one gzip member; invalidation of a replacing import; DICOM archive; `GeneratedBy`; isolated batch failures.
- **DICOM and suggestions:** converter lookup and detection, series order, last rule wins, short keywords as words, the DICOM modality tag over the name, lookup table JSON, series dates, the latest-study rule, ids from folders, the orientation guess.
- **Wizard:** step gating, unassigned rows, one subject per input folder, the no-post-op question, refusal while a run is active.

---

## 13. References

- BIDS specification 1.9 (raw data and derivatives).
- NIfTI-1 data format (header fields `qform` / `sform`, methods 1–3).
- Li, Morgan, Ashburner, Smith, Rorden (2016). The first step for neuroimaging data analysis: DICOM to NIfTI conversion. *Journal of Neuroscience Methods*, 264, 47–56.
- Lead-DBS user guide: [https://netstim.gitbook.io/leaddbs](https://netstim.gitbook.io/leaddbs) (dataset preparation).
- Horn et al. (2019). Lead-DBS v2. *NeuroImage*, 184, 293–316 (pipeline context only).

---

*End of spec 01.*
