# Spec 04 — Normalization (Wyss DBS Localizer)

**Status:** Phase B; §5.1–§5.5 describe the implemented behaviour (method `ants_syn` only, engines `ants` and `wgpu`); the other method rows, §10 (re-apply only), §13 (tonemap) and the other Lead-DBS design text are not implemented (marked where they occur)  
**Depends on:** `01_bids`, `03_coreg`, `00_overview`

---

## 1. Purpose

Map **coregistered** anatomy from **anchor native** space to **template space** (from `spacedef.name` + `primary_template` stem). Produce **normalised** NIfTIs, **forward and inverse** warps, **method log JSON**, and optional **tonemap** of CT in template space. **Re-apply** old warp when user selects apply-only.

**Two “brain” concepts (do not conflate):**

| Name | What |
|------|------|
| **ants_scrf** (prefs) | Extra **ANTs SyN** stage with **subcortical mask** *inside* the same `antsRegistration` run — Schoenecker-style **refine-in-template** |
| **SCRF / brain shift GUI** | **Post vs pre** refined **linear** in native space — **`06` / brainshift spec**; runs **after** norm in orchestration if enabled |

This doc: **template warp + apply volumes**. **Brain shift GUI** = separate subsection pointer only.

---

## 2. Inputs / outputs

**Inputs:** `Subject.norm.*` path bases, `Subject.coreg.anat.preop[anchor]`, all coreg preop + postop paths to warp, `Prefs.norm_settings` (ants preset, fsl skullstrip, `ants_scrf`, `ants_use_fa` …), `spacedef`, `diary` flag.

**Outputs:**

- `norm.anat.*` NIfTIs, `space-<TemplateSpace>`.
- `norm.transform.forward_base` + `inverse_base` + **suffix** by method:  
  - SyN / EasyReg / SynthMorph / SPM→ITK: **`ants.nii.gz`**  
  - FNIRT: **`fnirt` + .nii/.gz**  
  - Schönecker 3-step affine: **`ants.mat`**
- `norm.log.method` JSON, optional diary.
- If post-op **CT** → **tonemap** norm pass (not implemented).

---

## 3. Gating (orchestrator)

- **Lock** on anchor **norm** path: if approved and not apply-only and not stale → **skip** entire `normalize` stage.
- **Re-run** if method string = apply / re-apply, or lock **0.5** (coreg changed, multispectral norm) — method-specific recompute flag from **method metadata** (reference uses `method('prompt')` pattern).
- After success: `gencheckreg(norm)`.
- **Post:** if `overwrite_approved` and brainshift dir exists → cleanup + **warn** rerun brain shift.

**Diary:** if enabled, log file under `norm.log` prefix with timestamp.

### 3.1 Template resolution (Wyss DBS Localizer)

The template T1w is resolved once per run, first hit wins:

1. `--template <path|id>`;
2. environment variable `DBSLOC_TEMPLATE_T1` (a path or an id);
3. the shipped default `MNI152NLin2009bAsym_res-1mm`: `resources/templates/space/MNI152NLin2009bAsym/res-1mm/t1.nii.gz` (T2w beside it) in the installation or source tree. The files are committed (ICBM 2009b, every second voxel of the 0.5 mm original, licence beside them and in `NOTICE`; built by `scripts/build_shipped_template.py`).

**Catalogue ids.** A value of 1 or 2 that equals a catalogue id (`<space>_res-<label>`, label `1mm` or `0.5mm`, e.g. `MNI152NLin2009bAsym_res-0.5mm`), or a space name that maps to exactly one catalogue entry (`MNI152NLin2009bAsym` maps to the shipped 1 mm entry), resolves to the T1w of that entry: the shipped entry under `resources/templates/...`, downloadable entries under the per-user data folder `<data>/templates/space/<space>/res-<label>/{t1,t2}.nii.gz` (`DBSLOC_DATA_DIR`, else `%APPDATA%\dbsloc`, `~/Library/Application Support/dbsloc`, `$XDG_DATA_HOME/dbsloc` or `~/.local/share/dbsloc`). An entry that is not installed fails the stage with `template <id> is not installed; run: dbsloc template fetch <id>`. Anything else is a path. The catalogue lives in `crates/dbsloc-pipeline/src/template_catalog.rs` (the only place that names spaces; downloadable entries with an empty checksum are listed but refused; downloads: §3.2).

**Space.** Every resolved template carries a BIDS space id: a catalogue entry its own; for a path, the `"space"` string of a `template.json` beside the file, else the directory name that follows a `space` path component (`…/templates/space/MNI152NLin2009bAsym/…`), else `custom-<first 12 hex characters of the file's sha256>`. The id is written to the channel record (`template_space`, §5.2 step 6), the scene manifest `space` and Reco v2 `meta.template_space`.

An explicitly given path (1 or 2) is kept as given even when it is not a file; it never falls through to the next source, because normalizing to a different template than the one requested writes coordinates in the wrong space. Resolution itself never fails the run: `--plan` and stages that do not read the template are unaffected. Only stages that need the file fail — the `normalize` prerequisite (missing path: names it; nothing configured: names the shipped path, `--template <path|id>` and `dbsloc template list`) and the native reconstruction path, which reports the missing template as its blocker. Implementation: `crates/dbsloc-pipeline/src/templates.rs` (`select_template`, `missing_template_message`), applied once at the start of every run (`RunOptions::with_resolved_template`), so `dbsloc run`, the GUI and `dbsloc_pipeline_runner` resolve identically; the in-tree default is looked up under `$DBSLOC_REPO_ROOT`, else the source tree the extension was built from, else the ancestors of the running executable. The template's sibling `brainmask.nii.gz`, when present, is used by the reconstruction brain-mask gate (`specs/05` §4.5); a template without one simply skips that gate.

**T2 template** (second SyN channel, §5.2), resolved in the same pass, first hit wins:

1. `--template-t2 PATH` (`RunOptions.normalization_template_t2`);
2. environment variable `DBSLOC_TEMPLATE_T2`;
3. `t2.nii.gz`, then `t2.nii`, in the directory of the resolved T1 template (`--template …/t1.nii` → `…/t2.nii`), whatever the T1 file is called.

As for the T1 template, an explicit path (1 or 2) is kept as given even when it is not a file. Nothing found is not an error: normalize then runs T1-only (§5.2 step 5). An explicit T2 path that is not a file fails `normalize`, but only for a subject that has a pre-op T2w and with `norm_multispectral` on, with `normalize: T2 template <path> does not exist or is not a file; an explicitly given T2 template (--template-t2 or DBSLOC_TEMPLATE_T2) is never replaced by another source; pass --no-norm-multispectral to normalize on the T1w channel only`, before any tool runs. Implementation: `templates.rs` `select_template_t2`; the plan JSON (`dbsloc_reconstruct.plan_run`) reports it as `template_t2`, next to `norm_multispectral`.


### 3.2 Template catalogue downloads

Every catalogue entry except the shipped one is fetched on demand by one Rust implementation (`crates/dbsloc-pipeline/src/template_fetch.rs`) behind `dbsloc template list|fetch <id|space>` and the Normalize step's template picker in the GUI.

- **Source and pin.** The ICBM 2009 zip published by the Montreal Neurological Institute (URL, byte size and SHA-256 of the zip, and the exact member names of the T1w, the T2w and the licence per entry are fixed in the catalogue). An entry without a pinned SHA-256 is listed but refused.
- **Download.** HTTPS GET, streamed to `<data>/templates/space/<space>/.partial-<id>-<pid>/download.zip` while the SHA-256 is computed incrementally; progress is reported at most every 1 MB and cancellation is polled between chunks. The body is limited to the pinned size plus 1 MB.
- **Verification.** Size and SHA-256 must equal the pins; a mismatch fails with both values in the message and installs nothing.
- **Files written** to `<data>/templates/space/<space>/res-<label>/`: `t1.nii` and `t2.nii` (the two named members, written **uncompressed**, voxel data unchanged; amended 2026-10-25: float templates compress only about 1.1×, and an uncompressed file loads about 0.3 s faster at 0.5 mm; an install made before this amendment keeps its `.nii.gz` files, which every reader still accepts, `.nii.gz` before `.nii` as in the T2 rule above), `LICENSE` (the licence member verbatim) and `template.json` (`space`, `res_mm`, `entry_id`, `source_url`, `zip_sha256`, the three member names, `fetched_by`). Only the pinned member names are read from the archive; no other path is extracted.
- **Install.** Files are written into the staging folder and the folder is renamed onto the destination; an existing destination is renamed aside first and deleted only after the new one is in place. The zip is deleted after extraction. Failure or cancellation (`cancelled`) removes the staging folder and leaves any previous installation untouched.
- **GUI.** The template combo box lists the catalogue with the state of each entry (bundled, installed, download with size) and *Custom file…*; choosing an entry that is not installed shows *Download (NN MB)*, then a progress bar with *Cancel*; on success the entry is selected and a custom T2 template is cleared so the T2w beside the T1w is used. The download runs on a worker thread; closing the application cancels it.

---

## 4. Method dispatch (normative)

| `normalize_method` id | Class |
|------------------------|--------|
| `ants_syn` | ANTs `antsRegistration` multistage SyN + optional FA + seg assist + slab + `ants_scrf` stage |
| `apply` | Re-apply only `apply_normalization` (not implemented, §10) |
| `fnirt` | FSL `flirt` init + `fnirt` |
| `schoenecker_3step_affine` | ANTs: rigid + global affine + 2× masked affines (second/third step masks in template) |
| `spm_newseg` | Unified segmentation → **deformation** to ITK NIfTI |
| `spm_dartel` | DARTEL flow (high res, slow) — parameters in `Prefs` + template package |
| `spm_shoot` | SHOOT — same class |
| `easyreg` | FreeSurfer EasyReg **forward+inverse** nii.gz |
| `synthmorph` | SynthMorph **forward+inverse** nii.gz |

**Unknown method** → error `E_NORM_METHOD`, no partial writes. Wyss DBS Localizer implements `ants_syn` (default).

---

## 5. ANTs SyN (contract)

**Build** one `antsRegistration` CLI with **ordered stages**:

1. **Rigid** — per-channel MI/GC with preset convergence / shrink / smooth.  
2. **Affine** — same.  
3. **SyN** — whole brain, masks from `mask_fixed` / `mask_anatomy` if present, else `NULL` or brainmask if skull-stripped.  
4. **Optional slab SyN** — if **slab** FOV detected (voxel support heuristic vs anchor).  
5. **Optional `ants_scrf` SyN** — fixed mask = template **subcortical second-step** mask, moving mask from slab or NULL.

**Multispectral:** template–moving **pairs** with weights (default 1.25 per structural channel); optional **FA** half-weight; optional **segmentation** channels weight **20** and use **stricter** metric in builder. **Wyss DBS Localizer** implements T1w + T2w only, in the SyN stage only, weight 0.5 each (§5.2, the cohort-validated recipe); FA and segmentation channels are not implemented.

**Presets:** named bundle (e.g. “Effective: LowVariance, Default”) expanding to convergence strings — **data-driven table** in `Prefs`, not hardcoded in spec.

**Output rename:** engine writes `*Composite.nii.gz` and `*InverseComposite.nii.gz` → move to `forward_base+ants.nii.gz`, `inverse_base+ants.nii.gz`.

**Threads:** `ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS` from prefs.

**FA path:** if enabled, require paths to **FA in anchor space** and template **fa.nii**; **if paths broken** (reference bug) → feature **off** or **E_FA** in Wyss DBS Localizer.

---

### 5.1 Wyss DBS Localizer `ants_syn` stage (normative for Wyss DBS Localizer)

One implementation, `crates/dbsloc-pipeline/src/stage_impl.rs` `run_normalize` (unification phase 2). The registration and apply calls below are made by the bundled ANTs processes (`--reg-backend ants`); `--reg-backend wgpu` makes the same calls in-process on the GPU (§5.4), and `auto` (the default) picks one:

1. **Moving** = anchor of `specs/02` §4.3 (N4 output when on disk), **fixed** = the resolved template (§3.1). Outputs per `01` §7a (anchor in template space, `from-anchorNative_to-<Space>` / `from-<Space>_to-anchorNative` composites; ANTs writes to a short work prefix in the staging); result token `ok:normalization:ants_syn:<normalized anchor image stem>`.
2. **Isotropic registration grid, default = the template's own resolution** (`RunOptions.norm_isotropic_mm`, type `NormGrid::{Template, Mm(f64), Full}` in `dbsloc-protocol`; CLI `--norm-isotropic-mm MM|full|template`; environment `DBSLOC_NORM_ISOTROPIC_MM`, same values, the flag wins; GUI *Registration resolution*, default entry *Template resolution (<V> mm, recommended)*; API `RunRequest.norm_isotropic_mm` / `norm_full_grid`, both absent = the default). `MM` is a finite number of millimetres from 0.3 to 3.0 (`NORM_ISOTROPIC_MM_RANGE`; other values fail with `expected a number of millimetres from 0.3 to 3.0, 'full' or 'template', got '<value>'`); `full` (case-insensitive) means no resampling: SyN runs on the full template grid as stored; `template` (or no choice at all) is the **default, decided 2026-10-15**: the registration grid is the resolved T1 template's finest voxel edge rounded to 0.01 mm (`norm_grid.rs` `template_default_mm`: 1.0 mm for the shipped template, 0.5 mm for the 0.5 mm catalogue templates, an anisotropic template resamples to its finest axis), with `DEFAULT_NORM_ISOTROPIC_MM` = 1.0 used only when the template header cannot be read (the stage then fails on the template itself). **Capacity step (GPU only):** when the registration runs on the GPU (`--reg-backend wgpu`, or `auto` resolving to the GPU) and `fastwarp_gpu::plan_capacity` (§5.4) rejects the template's grid for this subject's channel count, the default steps to the finest coarser grid that fits, trying the template voxel size, then 0.1 mm steps up to 1.0 mm, then 1.5, 2.0, 2.5 and 3.0 mm (`norm_grid::step_candidates`); when none fits the template grid stays and the capacity gate of §5.4 applies. The step is recorded as a plan warning (`RunPlan.warnings`, plan JSON `warnings`, `dbsloc: warning: normalize: registration grid stepped from the template's <V> mm to <MM> mm: the GPU cannot fit the <V> mm grid (<capacity error>); choose --norm-isotropic-mm or --reg-backend ants to override`, printed once before the first stage and again by the stage), in the method log (`norm_grid_source: "capacity"`, `norm_grid_note`) and in the GUI below the picker (non-blocking, hover = the capacity error). With the ANTs backend there is no capacity model: the default is the template resolution. **An explicit choice (`MM` or `full`) is never changed**; the capacity gate of §5.4 handles it as before. The plan JSON carries the effective grid as `norm_isotropic_mm` (number or `null`) and `norm_grid_source` (`explicit` | `template` | `capacity`); the method log records the same two keys plus `norm_grid_note`, and its `norm_isotropic_mm` stays the effective number or `null` (consumers such as the checkreg slides and the lock comparison read the effective value). In the `RunOptions` JSON (CLI, `dbsloc_pipeline_runner`, `dbsloc_reconstruct.run_pipeline` / `plan_run`) a **missing** `norm_isotropic_mm` (or the string `"template"`) means the default, a number an explicit grid and an explicit `null` the full template grid; serialisation omits the key for the default. The space bridge (§5.4a) resolves the same choice against the target template. An invalid flag or environment value (`0`, negative, non-finite, any other word) exits 2 before the pipeline runs, with `expected a positive number of millimetres, 'full' or 'template'`. **Why the template resolution:** end to end on the Lead-Tutor cohort (§5.2 table) the T1w + T2w SyN on a 1.0 mm grid has a mean error of 1.031 mm on the 9 T2w subjects and 1.092 mm on all 10, against 1.088 / 1.201 mm on a 1.5 mm grid; the shipped template is 1.0 mm, so its default is unchanged, and a user who installs a finer template asked for that detail. Cost at 1.0 mm (T1w + T2w): `antsRegistration` ~3.6 GB private memory and 45–60 min per subject on 4 ITK threads (~25 min at 1.5 mm). The full template grid (0.5 mm for MNI152NLin2009bAsym) is **not measured**; memory and time scale with the voxel count, 8× that of the 1.0 mm grid, which is why the GPU capacity step exists. Unless `full` is chosen, moving and template are resampled into `normalization/work/moving_is<MM>mm.nii.gz` and `template_is<MM>mm.nii.gz` (removed at stage end), `<MM>` spelled as a Python float (`1.0`, `1.5`). Grid: origin and direction cosines kept; per axis `floor((n − 1) · spacing / MM + 1e-9) + 1` voxels, output voxel `i` samples source index `min(i · MM / spacing, n − 1)` trilinearly (`resample.rs`). An image whose spacing already equals `MM` on every axis (within 1e-6 mm) is used as is and not copied; an image coarser than `MM` is upsampled onto the `MM` grid (use `full` for a template coarser than the grid, as the synthetic end-to-end test does with its 3 mm template). A resample failure fails the stage; it never falls back to full resolution. `MM ≤ 0` is an error. **Finer than the template (2026-10-14):** `MM` is absolute, not a factor of the template's voxel size; an `MM` below the template's finest axis spacing (rounded to 0.01 mm, by more than 1e-6 mm) only interpolates the template, adding no detail but voxels. It is allowed, but planning (`plan_run`, `RunPlan.warnings`, plan JSON key `warnings`, copied into the `dbsloc run` JSON payload) records `normalize: registration grid <MM> mm is finer than the template's <V> mm voxels; the template is only interpolated, which adds no detail but costs memory and time (choose <V> mm or full)` (`<V>` = finest template spacing rounded to 0.01 mm, the comparison uses the rounded value; both spelled as Python floats) when `normalize` is planned, and the run emits it once as `dbsloc: warning: …` before the first stage. The GUI reads the resolved template's voxel size, shows it on *Full template resolution (<V> mm)*, disables presets finer than it, sets the *Custom…* minimum to it, warns when a saved value is finer, and shows the registration grid size in voxels.
3. **antsRegistration** argv `build_ants_syn_argv`: Rigid + Affine (MI) + SyN (CC), winsorize `[0.005,0.995]`, histogram matching off, `--write-composite-transform 1`, `--random-seed 20260910`, then the `--ants-…` pass-through tokens. With a pre-op T2w and a T2 template the SyN stage carries a second channel (§5.2, `build_ants_syn_multispectral_argv`); the steps here are otherwise unchanged.
4. **Post-op CT into the template** (strict): when the subject has a raw post-op CT, `antsApplyTransforms -d 3 -i <coregistered CT> -r <template> -o <short work name> -n Linear -t <forward composite>` (committed as the CT in template space, `01` §7a) on the **original** template grid (roadmap 1.5), followed only by the pass-through tokens `antsApplyTransforms` accepts: `--float`, `--verbose`, `--default-value`, `--output-data-type`, `--input-image-type`, each with the values that follow it (`ants_argv::apply_transforms_extra`); every other `--ants-…` token is meant for `antsRegistration` and is dropped with its values. The same applies to every post-op MR image. With the ANTs backend the prerequisite check requires `antsApplyTransforms` (`normalize: antsApplyTransforms not found (bundled or PATH)`) when the subject has a post-op image. A missing coregistered CT (`no coregistered CT (*_space-anchorNative_desc-preproc_CT.nii.gz); run coreg or check CT/anchor`), a missing composite or a failed apply fails the stage. Subjects without a post-op CT skip this step.
5. **Kill by signal 9** (OOM) of either tool is reported with the mitigation hint (coarser `--norm-isotropic-mm`, `--ants-float 1`, `--ants-max-threads 1` / `DBSLOC_ANTS_MAX_THREADS=1`, swap, smaller template); other failures as `<tool> failed with <code>`.
6. **Checkreg gate** per `specs/03` §12.4a when enabled.
7. **Outputs** (staged as one set, `00` §5.2, names per `01` §7a): the anchor in template space, the forward and inverse composites, every post-op image in template space, the method log `normalization/log/sub-<id>_desc-normmethod.json` (§5.2 step 6) and the T2w coregistration of §5.2 step 2 (image + transform). The T2w coregistration is **always** part of the staged set: a multispectral run writes it, a T1-only run produces neither file, so committing it removes the T2w coregistration of an earlier multispectral run (the checkreg slides then show no stale T2w pair). A failure anywhere in the stage, including the SyN after a successful T2w coregistration, keeps every previous output, the T2w coregistration included.

### 5.2 Multispectral (T1w + T2w) SyN (normative for Wyss DBS Localizer)

**Why.** Measured 2026-10-05/06 on the Lead-Tutor cohort; error = mean of head and tail distance to the expert markers, per lead; CORE distal bloom 0.5 mm (`specs/05` §4.2). Two sources: the *experiment script* rows share one coregistration run (`specs/03` §5.1) across their arms; the *full pipeline* rows are end-to-end `dbsloc run` results that share a different coregistration run across their two arms. Compare rows only within one source.

| SyN channels | Registration grid (`norm_isotropic_mm`) | Source | Mean error, 9 T2w subjects (mm) | Mean error, all 10 subjects (mm) |
| ------------ | ---------------------------------------- | ------ | ------------------------------- | -------------------------------- |
| T1w | 1.5 mm | experiment script | 1.415 | — |
| T1w | 1.0 mm | experiment script | 1.374 | — |
| T1w + T2w | 1.5 mm | experiment script | 1.224 | — |
| T1w + T2w | 1.5 mm | full pipeline, end to end | 1.088 | 1.201 |
| **T1w + T2w** | **1.0 mm** | **full pipeline, end to end** | **1.031** | **1.092** |

The tenth subject has no pre-op T2w and runs T1-only (step 5): 2.50 / 1.94 mm per lead at 1.5 mm, 1.80 / 1.49 mm at 1.0 mm. Repeat runs of the experiment script agree to ~0.01 mm. Adding the T2w channel gains more than refining the T1-only grid from 1.5 to 1.0 mm, at the cost of one rigid MR→MR run and a second CC metric; refining the multispectral grid from 1.5 to 1.0 mm gains a further 0.06 mm on the T2w subjects and 0.11 mm on all 10, which is why 1.0 mm is the default (§5.1 step 2).

**Algorithm** (`crates/dbsloc-pipeline/src/stage_impl.rs` `plan_norm_channels`, `normalize_with`):

1. **Channel choice.** Multispectral when all of: `RunOptions.norm_multispectral` (default `true`; CLI `--no-norm-multispectral` turns it off), the subject has a pre-op **T2w** that is not the anchor (`Subject.preop_raw["T2w"]`, i.e. the first T2w by the `01` §10 ordering rule; a subject whose anchor is the T2w has none), and a T2 template resolves (§3.1). Otherwise T1-only (step 5).
2. **T2w → anchor, rigid** (the `03` §4 step 2 pair, run inside `normalize`): moving = the raw T2w, fixed = the anchor the stage actually uses (`02` §4.3, N4 output when present). Argv `build_ants_mr_coreg_argv` = the full CT rigid run of `03` §5.2 with the centre-of-mass initialiser `[fixed,moving,1]` and **no** mask and **no** multi-start (both images are MR of the same session): `--dimensionality 3 --output [<pfx>,<pfx>_Warped.nii.gz] --initial-moving-transform [fixed,moving,1] --transform Rigid[0.1] --metric MI[fixed,moving,1,32,Regular,0.25] --convergence [1000x500x250x100,1e-6,10] --shrink-factors 8x4x2x1 --smoothing-sigmas 3x2x1x0vox --random-seed 20260910`, then the pass-through tokens. `<pfx>` = a short work prefix in the staging; the outputs are committed as the T2w in anchor space and its `from-T2w_to-anchorNative` transform (`01` §7a, `03` §4 step 2). A run that writes no warped image fails the stage (`normalize: T2w coregistration wrote no <name>`).
3. **Isotropic grid.** Unless the full template grid is chosen (`norm_isotropic_mm` = `null`, §5.1 step 2), the warped T2w and the T2 template are resampled exactly like the T1 pair (§5.1 step 2, same resampler, same no-op rule) to `normalization/work/moving2_is<MM>mm.nii.gz` and `template2_is<MM>mm.nii.gz`. The T2 template must be 3-D as well.
4. **SyN argv** `build_ants_syn_multispectral_argv`: identical to the T1-only argv of §5.1 step 3 (rigid and affine stages unchanged, T1 MI only), except that `SyN[0.1,3,0]` is followed by two metrics, in this order, each weighted **0.5**: `--metric CC[<T1 template>,<T1 moving>,0.5,4] --metric CC[<T2 template>,<T2 moving>,0.5,4]` (the T1-only argv carries one `CC[…,1,4]`). The composite is applied to the post-op CT as in §5.1 step 4.
5. **Fallback.** No non-anchor T2w, no T2 template, or `norm_multispectral` off → the T1-only argv of §5.1, byte-identical to before, and a log note `dbsloc: note: normalize: <anchor> channel only: <reason>` with reason `no pre-op T2w besides the <anchor> anchor`, `no T2 template (pass --template-t2, set DBSLOC_TEMPLATE_T2, or place t2.nii[.gz] beside the T1 template)` or `multispectral normalization disabled (--no-norm-multispectral)`. An explicit T2 template that is missing is an error, not a fallback (§3.1).
6. **Method log** `normalization/log/sub-<id>_desc-normmethod.json` (written every completed run, both modes; `approval: 0`, `engine`, `custom: false`, `version`, plus the former channel record): `method` (`"ants_syn"`), `channels` (`["<anchor>", "T2w"]` or `["<anchor>"]`), `moving` and `template` (absolute paths, one per channel, in channel order; the T2 moving image is the warped T2w), `syn_cc_weights` (`["0.5","0.5"]` or `["1"]`), `t2_source`, `t2_coreg_transform`, `t2_coreg_warped` (absolute paths, `null` when T1-only), `norm_isotropic_mm` (number or `null`), `template_space` (§3.1) and, T1-only, `note` = the step 5 reason. A fresh run therefore resets the QC approval; the checkreg viewer (`03` §12.3.4) approves it again. The fresh log carries no `reviewed` key: `approval: 0` without `reviewed: true` means *not reviewed yet* (`03` §12.4a step 5).
7. **Lock fingerprint.** With `--fingerprint-locks` the `normalize` inputs are the anchor plus the raw non-anchor T2w when present (`00` §6).
8. **Checkreg.** The norm gate shows the T2w → anchor pair when the channel record lists `T2w` (`03` §12.4a).

### 5.3 (removed 2026-10-08)

The experimental `fireants` normalization method was removed on 2026-10-08. Measured on the
Lead-Tutor cohort, its best configuration reached a mean marker error of 6.6 mm (`docs/PHASE_C_ROADMAP.md`
item 3.16), so it is not usable as configured. `--norm-method` now accepts `ants_syn` only, and `"fireants"` fails parsing with
`E_NORM_METHOD`.

### 5.4 Execution backend `--reg-backend auto|wgpu|ants` (normative for Wyss DBS Localizer)

**What it is.** The `ants_syn` method of §5.1–§5.2 can run its registration calls in two ways: as the bundled ANTs processes (`ants`, the path of §5.1 unchanged) or in-process on the GPU (`wgpu`), with the engine of `fastwarp/crates/fastwarp-reg` (CPU reference in `f64`, the ITK / ANTs algorithms ported from the Apache-2.0 sources, `fastwarp/docs/THIRD_PARTY_PORTS.md`) running its loops on `fastwarp/crates/fastwarp-gpu` (`WgpuBackend`, WGSL kernels, `f32`; Vulkan, DirectX 12 or Metal). Measurements: `docs/gpu_syn_cohort_report.md`. Implementation: `crates/dbsloc-pipeline/src/reg_backend.rs` (option, probe, resolution), `norm_native.rs` (the stage flow), `stage_impl.rs` `normalize_with`.

**Selection.** `RunOptions.reg_backend` = `"auto"` (serde default: a missing JSON key means `auto`, so old option files and persisted GUI state keep working) | `"wgpu"` | `"ants"`; any other value is rejected when the options are parsed. CLI `dbsloc run --reg-backend auto|wgpu|ants`; environment `DBSLOC_REG_BACKEND` (same values; the flag wins; an invalid flag or variable exits 2; `dbsloc_pipeline_runner` uses the variable only when its JSON leaves the key out, `specs/00` §5.2, and exits 2 on an invalid one); GUI: one *Engine* combo on each of the Preprocess, Coregister and Normalize steps (`Auto (GPU when available)`, `GPU (wgpu)`, `ANTs (CPU)`), persisted with the pipeline settings (a legacy single *Registration engine* setting seeds all three on load) and sent as the three per-step overrides below, plus one *Number of threads* slider in the run options and a status line naming the detected adapter and the self-test verdict (computed once on a worker thread at start-up, never on the paint thread). **Per-step overrides.** `RunOptions.reg_backend` is the run-wide default. Three optional fields override it for one step each (`reg_backend_preprocess`, `reg_backend_coregister`, `reg_backend_normalize`; values as above; omitted from the JSON when unset, a missing key means no override, so old option files keep working), read through `RunOptions::backend_for(stage)`: `preprocess` (N4) and `precoreg` use the preprocess override; `coreg` (including the multi-start) uses the coregister override; `normalize`, the space bridge (`specs/04` §3.1), the normalization-grid capacity planning and the point warps of §5.4 (*Point warps*) use the normalize override; every other stage uses the default. All requirements of this section (resolution rule, prerequisites, capacity gate, records, locks) apply per step to that step's own request and record: a stage's lock check compares the recorded `reg_backend` with the backend requested for that stage only. CLI `dbsloc run --preprocess-backend|--coregister-backend|--normalize-backend auto|wgpu|ants` with environment `DBSLOC_PREPROCESS_BACKEND`, `DBSLOC_COREGISTER_BACKEND`, `DBSLOC_NORMALIZE_BACKEND` (same precedence and validation as `--reg-backend`; the flag wins; an invalid value exits 2); `dbsloc reco correct` accepts `--normalize-backend` (its only registration use is the point warp). `--reg-backend` keeps meaning "every step" when no per-step flag is given.

`dbsloc gpu probe [--json]` (`dbsloc_reconstruct.gpu_probe()`) prints the same probe (adapter, graphics API, device type, driver, self-test attempts, verdict, `auto selects: wgpu|ants`) and exits 1 when no usable GPU exists.

**Variable names.** The GPU variables of this section are read by `fastwarp-gpu` as `FASTWARP_GPU_<NAME>` (`FASTWARP_GPU_BACKEND`, `_ADAPTER`, `_ALLOW_SOFTWARE`, `_MEMORY_BUDGET_MB`); the former `DBSLOC_GPU_<NAME>` spelling used below is still read when the `FASTWARP_` one is unset. The adapter requirement is test-only: `fastwarp-gpu` reads `FASTWARP_GPU_REQUIRE_ADAPTER` (with the `DBSLOC_` fallback), and the dbsloc VTA tests read `DBSLOC_GPU_REQUIRE_ADAPTER`.

**Adapter probe.** `probe_gpu()` creates a device on the adapter that `DBSLOC_GPU_BACKEND` (`auto|vulkan|dx12|metal`; `auto` = Vulkan before DirectX 12 on Windows, Metal on macOS, Vulkan elsewhere), `DBSLOC_GPU_ADAPTER` (case-insensitive name substring) and `DBSLOC_GPU_ALLOW_SOFTWARE` select, compiles the kernels and runs the known-answer self-test of `WgpuBackend::new` separately on the SyN and the Mattes MI kernel families, recording the attempts each needed (`self_test_attempts_syn`, `self_test_attempts_mi`); a mismatch on a cold shader cache, seen on DirectX 12, recompiles that family and retries up to twice (`SELFTEST_ATTEMPTS = 3`), and a third mismatch fails the probe. An invalid `DBSLOC_GPU_BACKEND` is `E_GPU_BACKEND` (valid: `auto`, `vulkan`, `dx12`, `metal`): `dbsloc gpu probe` prints the error, and a run fails before any work. Verdict from `GpuCaps::verdict`: `GO`, `BORDERLINE` (another device requirement fails, DirectX 12 without subgroups, or a software adapter) or `NO-GO` (largest storage binding at most 256 MiB; the probe is unavailable). **Software rasterisers are never used** unless `DBSLOC_GPU_ALLOW_SOFTWARE=1` (`WGPU_FORCE_FALLBACK_ADAPTER=1` alone, as CI sets it, leaves no eligible adapter). A panic in a graphics driver during the probe is caught and reported as unavailable. **API fallback:** each candidate graphics API of the selection is tried in order (`auto` on Windows: Vulkan, then DirectX 12); when device creation or the known-answer self-test fails on one, the next is tried, and the error lists every attempt when all fail (`GpuContext::new_with`). An explicit `DBSLOC_GPU_BACKEND` has one candidate and no fallback.

**Device lifetime.** One device per process, created by the first probe and shared by the registration and N4 engines of every later stage. A device the driver reports **lost** is replaced by the next stage that asks for it (a new device on the same adapter selection; when it cannot be created, `auto` falls back to ANTs with that reason and `wgpu` fails with `the GPU device was lost and could not be created again: …`). After every `preprocess`, `precoreg`, `coreg` and `normalize` stage, completed or not, the pipeline releases the device buffers the engine keeps pooled between runs (`reg_backend::release_idle_gpu_memory`); buffers of live images and fields are untouched, so a long GUI session does not keep a finished normalization's device memory.

**Resolution rule** (`reg_backend::resolve`): `ants` → ANTs. `wgpu` → the GPU, or the error `E_REG_BACKEND: the registration backend wgpu was requested but no GPU can be used: <adapter problem>; pass --reg-backend ants to use the CPU path` (raised by the prerequisite check, before any work, and again by the stage). `auto` → the GPU when the probe is available, otherwise ANTs with the note `dbsloc: note: normalize: registration backend ants: <why>` and the same text in the record (`reg_note`). A device that probed fine but cannot be created for the run falls back under `auto` and is an error under `wgpu`. Tests: `reg_backend::tests` with an injected probe.

**Stage flow with `wgpu`** (same staging as §5.1 step 7, `00` §5.2; no ANTs process runs in this stage):

1. Inputs and template resolution as §5.1 step 1; the anchor, template (and T2 pair) must be 3-D (`need 3D …`).
2. Multispectral (§5.2): **T2w → anchor** rigid on the ported engine: centre-of-mass initialiser, `Rigid[0.1]`, `MI[…,1,32,Regular,0.25]`, `[1000x500x250x100,1e-6,10]`, `8x4x2x1`, `3x2x1x0vox`, seed 20260910 (`LinearRecipe::cohort_rigid`, the recipe of `build_ants_mr_coreg_argv`), on the raw images (no winsorising, as in that argv; the T2w image is not windowed, unlike the CT of `specs/03` §5.1), full resolution, the Mattes MI metric on the GPU. Every image the stage loads (anchor, template, T2w, T2 template) has its non-finite voxels (NaN, ±inf) replaced by 0 first, the same loader as `coreg` (`stage_impl::load_reg_volume`), so a NaN cannot poison the centre of gravity. Outputs `sub-<id>_T2w_to_<anchor>0GenericAffine.mat` (collapsed affine, ITK `.mat`) and `_Warped.nii.gz` (linear, default value 0, `f32`, on the anchor grid), then rounded to `f32` before the registration like a file round trip. The record keeps `reg_t2_coreg: "wgpu"` and the per-level traces.
3. **Registration** `fastwarp_reg::register::register_full`: isotropic resample of every channel (the pipeline resampler rule of §5.1 step 2, `norm_isotropic_mm`, results rounded to `f32`), `PreprocessImage` (winsorise `[0.005,0.995]`, rescale to `[0,1]`), centre of mass → `Rigid[0.1]` → `Affine[0.1]` (first channel only, as the argv has one `MI`), collapsed to one affine as ANTs does, then `SyN[0.1,3,0]` with `CC[…,w,4]` per channel (`w` = 1 T1-only, 0.5 + 0.5 multispectral; `[100x70x50x20,1e-6,10]`, `8x4x2x1`, `3x2x1x0vox`) and the symmetric inversion schedule (`SynRecipe::cohort` / `cohort_multispectral`). The resampling happens in memory: the GPU path writes **no** resampled registration pair to disk. The checkreg gate (`specs/03` §12.4a step 1) pairs the warped anchor with the template resampled onto the recorded grid, and resamples the template itself into `normalization/work/template_is<mm>mm.nii.gz` (`checkreg.rs`, the §5.1 step 2 resampler; the directory is removed once the review is approved) on either backend. Every CPU part of the stage (resample, preprocessing, the host side of the linear and SyN stages, the T2w rigid, apply) runs on one dedicated rayon pool of `ants_max_threads` workers when that option is set (`reg_backend::with_worker_threads`, shared with `coreg`, `preprocess` and the reconstruction point warp). The linear stages and the SyN stage share the smoothed copies of the first channel (same preprocessed images, same sigmas, same units; `SmoothCache`, keyed by sigma, unit and the identity of the image pair, so the SyN stage never smooths again what the linear stages already did and a cache handed another pair is emptied); results are bit-identical to smoothing twice; the linear stages hand the engine the cached copy (or, at sigma 0, the full-resolution image itself) without copying it, and the SyN stage moves its smoothed copies onto the backend without a further copy. **Cancellation (2026-10-16).** The run's cancel flag reaches every in-process registration call of `precoreg`, `coreg` and `normalize` (`RegEngine::register_cancellable` / `rigid_cancellable` / `rigid_from_cancellable`, `SynOptions::cancel`, `run_rigid_registration_cancellable` / `run_rigid_registration_from_cancellable`): the engine polls it at the start of every level of each linear stage, once per iteration of the conjugate-gradient optimiser (`optimise_level`), and at the start of every SyN level and iteration, so a cancel ends the call within one iteration (one metric evaluation plus its line search, or one SyN iteration). A set flag ends the call with `RegError::Cancelled`, which the pipeline maps to its cancelled outcome (`cancelled by user`, `reg_backend::cancelled_or`); a cancel is never reported as a GPU failure, so `auto` never falls back to ANTs on it, and the staged outputs of the stage are discarded as for any other cancel. Between the phases the stage checks the flag as before.
4. **Outputs under the same names as the ANTs path** (`01` §7a): forward composite = `[Affine, Warp]`, inverse composite = `[InverseWarp, Affine⁻¹]` (ITK HDF5, `float32` fields, the layout `fastwarp-reg` and ANTs both read), the anchor in template space (first moving channel through the forward chain onto the registration grid, `f32`). The composite just written is read back once and checked (`normalize: the composite just written cannot be read back: …`); the CT apply of step 5 uses that same read-back chain.
5. **Post-op CT** onto the **original** template grid (§5.1 step 4) with `fastwarp_reg::apply::apply_nifti`: linear interpolation, default value 0, composite read back from the staged file (the transform ANTs would read), written as `f32` NIfTI. (`antsApplyTransforms` keeps the input pixel type; the Rust apply always writes `f32`; consumers load through scaling, so values agree to `f32` rounding.) The CT apply runs on the CPU (rayon worker threads, capped by `ants_max_threads`), not the GPU.
6. Failures: a registration or apply error → `normalize: <what> failed on <adapter label>: <error>` (an out-of-memory error adds `; retry with a coarser --norm-isotropic-mm or --reg-backend ants`); a failure anywhere keeps every previous output (`outputs.rs` staging). **Fail fast:** before any image is loaded, `fastwarp_gpu::plan_capacity(grid, channels, caps)` (`fastwarp/crates/fastwarp-gpu/src/capacity.rs`) checks the largest single storage binding (the three-plane displacement field, `n·3·4` bytes; the five-plane CC buffer was split into five `n·4` bindings, roadmap 3.18 WP8) and the finest level's total device allocation against `max_storage_buffer_binding_size` and the memory budget (`DBSLOC_GPU_MEMORY_BUDGET_MB` when set; else the device memory the graphics API reports — DX12 local budget, Vulkan largest device-local heap — halved on integrated and software adapters, or `max_buffer_size` capped at 4 GiB when none is reported; minus 1 GiB reserved for the viewers). With `wgpu` a grid that cannot fit fails with `the finest registration level <dims> needs a storage binding of <need> bytes (the three-plane displacement field), but the device binds at most <limit> bytes per storage buffer; use --norm-isotropic-mm 1.0 or --reg-backend ants`; with `auto` it falls back to ANTs and the reason goes into `reg_note` (the same remedy is appended when a binding-limit error surfaces later). The split layout removes the 0.5 mm binding blocker — the largest binding is the 833 MB three-plane field against the 1 GiB limit, and the 0.5 mm grid passes the binding check but is rejected by the memory budget (8 GiB on the test Intel Arc) — so the practical limit on the Arc is **device memory**: a 0.5 mm cohort run reaches the finest SyN level and then fails allocating the 833 MB field (`out of memory allocating 832827744 bytes for field`), even with `DBSLOC_GPU_MEMORY_BUDGET_MB=16000`, while 0.8 mm runs (cohort mean 1.141 mm); the deliverable is the measurement, not a default change (roadmap 3.18 WP8, `docs/gpu_syn_cohort_report.md`). The `k_cc_deriv` kernel binds 8 storage buffers, at `max_storage_buffers_per_shader_stage`.

**Prerequisites** (`prereqs.rs`): with `wgpu`, or `auto` that resolves to the GPU, the `antsRegistration` and `antsApplyTransforms` checks of `normalize` are skipped (the `coreg` check follows the `coreg` backend, `specs/03` §5.1, so it is skipped too when that stage resolves to `wgpu`; the `preprocess` N4 check follows the `preprocess` backend, `specs/02` §4.3, so it is skipped when that stage resolves to `wgpu`); the template check is unchanged.

**Records.** The method log of §5.2 step 6 gains, for every `ants_syn` run: `reg_backend` (`"wgpu"` or `"ants"`, the backend that ran), `reg_requested` (`"auto"`, `"wgpu"`, `"ants"`) and, when `auto` fell back, `reg_note`. With `wgpu` also `reg_adapter` (`name`, `backend`, `device_type`, `driver`, `vendor_id`, `device_id`, `verdict`, `self_test_attempts`, `self_test_attempts_syn`, `self_test_attempts_mi`), `reg_t2_coreg` (`"wgpu"`, or `null` T1-only), `reg_timings` (`load_s`, `t2_coreg_s`, `resample_s`, `preprocess_s`, `linear_s`, `syn_s`, `warp_s`, `write_s`, `ct_apply_s`, `total_s`), `reg_syn_levels`, `reg_rigid_levels`, `reg_affine_levels`, `reg_t2_coreg_levels` (per-level grid, iterations, convergence, final value) and `reg_affine_parameters`. The stage logs `[normalize] registration backend wgpu on <adapter label>` and the phase times. A record without `reg_backend` was made by `ants`.

**Locks** (`approved_outputs_match`, after the approved-output matching of §5.5; the first difference logs `dbsloc: note: normalize: <why>; the stage runs`): `auto` accepts approved outputs made by either backend; an explicit `wgpu` or `ants` re-runs when the record's `reg_backend` differs (`the approved outputs were made with the <have> registration backend, this run asks for <want>`; no key = `ants`). No record never forces a re-run for the backend. **Registration grid (2026-10-15):** the grid comparison of §5.5 applies only to an explicit choice (`NormGrid::Mm` / `Full`); when the grid was not chosen (`NormGrid::Template`, the default) the recorded `norm_isotropic_mm` is never by itself a reason to re-run, so approved outputs made at 1.0 mm on a finer template survive the change of default. Tests: `stage_impl::tests::approved_outputs_follow_the_registration_backend_rule`, `stage_impl::tests::a_grid_that_was_not_chosen_keeps_approved_outputs_on_any_grid`, `driver_tests::approved_normalize_reruns_when_the_registration_backend_differs`, `driver_tests::approved_normalize_reruns_when_grid_or_template_changed`.

**Point warps.** Reconstruction maps native points to template space through the inverse composite (`05` §6). `ants` keeps `antsApplyTransforms` on a coordinate image. Otherwise (`RunOptions::backend_for(Stage::Normalize)` resolves to `auto` or `wgpu`) `point_warp.rs` `RustPointWarp` reads `<prefix>InverseComposite.h5` with `fastwarp-reg` and evaluates `fastwarp_reg::map_points_ras_lattice`, which **reproduces that coordinate-image route exactly**: the chain is evaluated at the integer-millimetre lattice nodes around each point (`f32`-rounded, as the written coordinate image holds them) and blended trilinearly, so numbers stay compatible with ANTs-produced ones, and it runs on the CPU (rayon), not the GPU. When the channel record says `reg_backend: wgpu` an unreadable composite is an error (`E_RECON_warp: native to template: …`); otherwise an unreadable composite falls back to ANTs with a `dbsloc: note: reconstruct: …` line. The log says `[reconstruct] point warp: fastwarp-reg through InverseComposite.h5` when the Rust route ran. The template-space reconstruction of post-op MRI (`05`) works in template space and warps no points. Evidence: the same route reproduces the stored ANTs template coordinates of all ten Lead-Tutor subjects to below 8e-7 mm (`docs/gpu_syn_cohort_report.md`, column `repro`), and `point_warp::tests::rust_route_matches_the_coordinate_image_route_run_through_the_cpu_apply` pushes a coordinate image through `fastwarp_reg::apply::apply_chain` (the port of `antsApplyTransforms`) on a non-linear composite written and read as `.h5` and gets the same points within 1e-4 mm.

**Known, intended deviations from ANTs** (results are reproducible per backend, not across graphics APIs): (1) `f32` arithmetic on the GPU for images, fields, window sums and reductions (host scalars `f64`), ANTs runs `double`; (2) fixed-order tree reductions and direct 9-tap sums instead of ITK's per-thread queues and compensated summation, mean-shifted intensities in the CC sums; (3) fused multiply-add contraction and transcendental functions differ per shader compiler; (4) window sums are computed once per channel per iteration and shared by both SyN directions (mathematically identical); (5) the metric sample jitter follows ITK's Mersenne-Twister call sequence as ported, so the rigid / affine stages agree at parameter level, not sample level; (6) fields are written as `float32` (ITK reads them either way); (7) the CT apply writes `f32` (above); (8) the GPU CC value and gradient skip a voxel whose window variance is at most `REL = 1e-6` times its raw sum of squares (`sff > REL·sii`, `smm > REL·sjj`, `syn_cc_value.wgsl` / `syn_cc_deriv.wgsl`), a guard against `f32` cancellation in flat regions that the CPU reference does not have: on the head phantom the CC energy differs from the CPU value by 0.5–3.3 %, which only moves the per-level convergence stop; accepted 2026-10-16 (cohort within D9, no re-run). Everything else (recursive-Gaussian pyramid, discrete Gaussian taps, shrink geometry, convergence monitor, inversion schedule, multi-metric weighting, collapse rules, formats) is ported, not approximated.

**Measured agreement** (Intel Arc 140V, Vulkan, 1.0 mm grid, Lead-Tutor cohort): SyN stage started from ANTs' own affine: contact coordinates move by 0.00065 mm on average, 0.0048 mm at most, over 200 contacts (field RMS 0.0000–0.0047 mm inside the brain, no folding, same Jacobian range; 27 s per subject). Whole chain without any ANTs process: contacts 0.012 mm mean, 0.063 mm maximum from the ANTs coordinates of the 1.092 mm run (192 points); collapsed affine of `57245` within 0.20 mm (maximum, mean 0.076 mm) of ANTs' over the brain; 28 to 54 s per subject against 45 to 60 min for ANTs. Through the integrated pipeline the Lead-Tutor marker error is 1.101 mm against 1.092 mm for ANTs (all 10 subjects, 1.0 mm T1w + T2w; acceptance mean at most 1.142 mm met, roadmap 3.18 WP6). With the preprocess N4 also in-process (WP11, `specs/02` §4.3) a run with `--reg-backend wgpu` starts **no** ANTs process, and the integrated-pipeline cohort mean is **1.076 mm** against the 1.092 mm ANTs baseline (Δ −0.017, D9 passes; `docs/gpu_n4_report.md`); nothing in a `wgpu` run remains on the bundled ANTs processes. **T2w → anchor against ANTs** (measured 2026-10-08, WP10): on nine of the ten Lead-Tutor subjects (all but the T1-only `76325`) the ported T2w rigid agrees with the ANTs T2w coregistration to at most 0.72 mm (at most 0.25 mm mean) over the anchor brain mask; `33544` is a different local optimum (5.59 mm maximum, 1.96 mm mean, matrix difference ≈ 2.7e-2). Injecting the ANTs T2w transform into the GPU normalize (`DBSLOC_T2_COREG_FROM`, the test input of `norm_native.rs`) restores the ANTs marker score on `33544` (+x 1.093 / −x 1.115 mm against 1.093 / 1.116), so the per-lead change is that optimum, not the SyN. The ANTs T2w argv omits `--winsorize-image-intensities` and `--use-histogram-matching`, but its `PreprocessImage` window `[min,max] → [0,1]` is a linear intensity map that does not change the data-driven Mattes binning, and toggling histogram matching leaves the ANTs transform bit-identical, so the difference is the accepted sampling / optimiser-path deviation (decision 8), not a preprocessing port error. **Still UNSPECIFIED / not yet measured:** the DirectX 12 and Metal paths on real data, and `--norm-isotropic-mm` below 1.0 on the GPU.

**Validated on.** Vulkan on the Intel Arc 140V (all acceptance runs above). DirectX 12 on the same Arc: 9/10 Lead-Tutor subjects, mean **0.993 mm** against the 0.994 mm Vulkan mean over those same 9 (Δ −0.001, D9 passes); `15454` is **refused** because the SyN `smooth_field` known-answer self-test failed on the DX12 driver after two freshly compiled attempts (relative error 0.82–2.64 against the 2e-6 tolerance), an intermittent miscompile of that kernel, so an explicit `wgpu` errors with `E_REG_BACKEND` rather than running a wrong field; two DX12 runs are byte-identical. NVIDIA Vulkan, NVIDIA DirectX 12 and Apple Metal are pending. Results and the run sheet: `docs/gpu_vendor_report.md`.

**Verification.** Rust: `reg_backend::tests` (parse, environment value, resolution rule, injected probe, fallback, software adapters excluded, a small registration on the real adapter when one exists, `capacity_gate` fallback under `auto`), `fastwarp-gpu` `capacity::tests` (1.0 mm fits the Arc binding, 0.5 mm fits the split layout, the boundary at the field binding, the split layout under injected caps), `device::tests` (`DBSLOC_GPU_BACKEND` parse and the invalid-value error), `gpu_kernels::mi_known_answer_matches_the_cpu_reference`, `options::tests`, `prereqs::tests`, `stage_impl::tests::native_normalize_*` (the whole stage on the CPU reference engine: outputs, record keys, CT apply, failure keeps outputs, cancel), `reg_backend::tests::a_set_cancel_flag_ends_every_engine_call_with_the_cancelled_outcome`, `precoreg::tests::a_cancel_during_the_native_rigid_is_the_cancelled_outcome`, `fastwarp-reg` `optimizer::tests::a_cancel_flag_stops_the_optimiser_at_the_next_iteration` and `register::tests::a_set_cancel_flag_stops_the_syn_stage_and_the_full_registration`, `point_warp::tests`, `ct_recon::tests` (route selection and fallback), `lib.rs driver_tests`; Python `tests/test_e2e_synthetic_wgpu.py` (slow; skipped without a hardware GPU adapter, or without bundled ANTs for the explicit-ANTs control run).

### 5.4a Template space bridge (normative for Wyss DBS Localizer)

**Purpose.** Assets bound to the reference space `MNI152NLin2009bAsym` (TRAC search windows, the AHEAD atlas, Lead-Tutor coordinates) must work with any template (decision of 2026-10-12: reference space `MNI152NLin2009bAsym`; another template is linked to it once by a cached SyN warp that maps space-bound assets in and coordinates out). When the resolved template's space (`template_catalog::infer_space`) is not the reference space, one registration with **fixed = the target template, moving = the shipped reference template** (`default_entry()`) is built once and cached. For the reference space there is no bridge (`ensure_bridge` returns `None`).

**Registration.** The same recipes as `normalize` (§5.1, §5.2, §5.4): Rigid → Affine → SyN on the `norm_isotropic_mm` grid (default 1.0 mm), T1w + T2w when both templates have a `t2.nii.gz` / `t2.nii` beside the T1 (no T2 → T1 step: a template's T1 and T2 already share a space), otherwise T1w only; engine per `--reg-backend` with the same `choose_for_run` and capacity-gate fallback to ANTs (`dbsloc: note: bridge: …`) and the same ANTs argv builders. Code: `crates/dbsloc-pipeline/src/space_bridge.rs`, sharing `norm_native::register_and_write` and `stage_impl::ants_registration_inputs` with `normalize`.

**Cache.** `<user data folder>/templates/bridges/<target space>__<first 12 hex of sha256(target T1)>/` holding `from-MNI152NLin2009bAsym_Composite.h5`, `…_InverseComposite.h5`, `…_Warped.nii.gz` and `bridge.json` (`reference_space`, `reference_t1`, `reference_t1_sha256`, `target_space`, `target_t1`, `target_t1_sha256`, `channels`, `grid_mm`, `backend`, `created`, `registration`). Built in a staging folder `.partial-<name>_<process id>_<counter>` beside it (unique per process and build, so two processes or two builds never share one) and renamed on success; a failed or cancelled build leaves no cache. A cache is valid only while both composite files exist and the two recorded sha256 values equal those of the current files (`space_bridge::cache_decision`); otherwise it is rebuilt. Log lines: `dbsloc: note: bridge: building <reference> -> <target> (once per template)` and `dbsloc: note: bridge: using cached <dir>`. `DBSLOC_NO_BRIDGE=1` disables the bridge (`ensure_bridge` returns `None` after a note); the synthetic end-to-end tests set it.

**When it runs.** At the end of a successful `normalize` (a failure there is `dbsloc: warning: bridge: …; reconstruction will lack reference-space coordinates`, a cancel there is `dbsloc: warning: bridge: cancelled; reconstruct builds it when it needs it`; neither fails the stage) and, instantly from the cache, when `reconstruct` writes the Reco file (`05` §8).

**Directions** (the ITK convention of `fastwarp-reg::points`: a chain maps output-space points to input-space points). With fixed = target and moving = reference, `Composite.h5` maps **target points to reference points** (`Bridge::points_to_reference`, RAS millimetres; Rust route `RustPointWarp` over `map_points_ras_lattice`, falling back to `antsApplyTransforms` on a coordinate image when `fastwarp-reg` cannot read the file) and resamples a **reference-space image onto a target-space grid** (`Bridge::warp_image_to_target`, `apply_nifti` or `antsApplyTransforms`, `nearest` for label images). `InverseComposite.h5` is the opposite point direction and is not used by consumers. `space_bridge::tests::points_and_images_move_in_the_same_direction` proves both against a pure translation: a blob in a reference image lands at the target position `q` with `points_to_reference(q)` equal to the blob centre within 0.5 mm.

**Affine summary.** `Bridge::affine_to_reference` returns the 4x4 (row-major, target mm → reference mm) least-squares 12-parameter affine fitted to `points_to_reference` on a 9×9×9 lattice, x, y ∈ [−70, 70] mm and z ∈ [−50, 80] mm (`affine_fit_to_reference` also returns the RMS residual, `AffineFit::rms_mm`). It is a coarse summary for window placement, not a substitute for the warp.

**Consumers.** (1) Reco `reference` block (`05` §8): `points_to_reference`. (2) TRAC search windows (`05` §4.3): `affine_to_reference` becomes `TracConfig::world_to_reference` for the template-space MRI path and the native-frame CT path; a bridge error fails `reconstruct`. (3) The scene atlas (`08` §5): the AHEAD masks are resampled onto the template grid with `warp_image_to_target` (linear) into `<bridge dir>/atlases/ahead/structures/`. (4) Lead-Tutor export (reads the `reference` block).

**Failure.** No installed reference template, no data folder, or a failed registration is an error of `ensure_bridge`; callers degrade as stated above (warning, no `reference` block) and never fail `normalize` or `reconstruct` for it. Consumers that need reference coordinates (Lead-Tutor) refuse a file without them (`05` §8).

### 5.5 Approved-output matching

An approved `normalize` lock skips the stage only while the outputs on disk were made as this run asks. The normalization method log (one per subject; it records the template) is compared field by field, in this order, and the first difference logs `dbsloc: note: normalize: <why>; the stage runs` and normalizes: `method` (`the approved outputs were made with <method>, this run asks for ants_syn`; no record counts as `ants_syn`, and any recorded method other than `ants_syn` re-runs); `norm_isotropic_mm` (`the approved outputs were made on a 1.5 mm grid, this run asks for a 1.0 mm grid`; `null` = `the full template grid`; compared as numbers within 1e-6 mm; only when the grid was chosen explicitly, `NormGrid::Mm` / `Full`: with the default `NormGrid::Template` the recorded grid is never a reason to re-run, §5.4 Locks); `template[0]`, the absolute T1 template path, against the requested template compared as `absolute()` paths (`… made for the template <path>, this run asks for <path>`); `template_space` against the space of the requested template, only when the record has the key (`… made in the template space <a>, this run asks for <b>`); the channel mode, from `channels` against what `plan_norm_channels` would pick now (`… made from T1w + T2w, this run asks for the anchor image only` and the reverse); and, when both are multispectral, `template[1]` against the T2 template (`… made with the T2 template <path>, this run asks for <path>`). **Records made before a key existed keep their approved outputs:** a record without `norm_isotropic_mm`, `template`, `template_space` or `channels` (or without any record at all) is never a reason to re-run for that field. This holds with and without `--fingerprint-locks`; with it, the input fingerprint of §5.2 step 7 applies as well. Downstream stages see the new outputs through their own fingerprints (`00` §6). The backend rule of §5.4 is part of the same check. Tests: `stage_impl::tests::approved_ants_outputs_match_only_an_ants_request`, `stage_impl::tests::approved_normalize_reruns_when_the_record_was_made_by_another_method`, `stage_impl::tests::approved_outputs_follow_the_registration_grid` / `_the_template` / `_the_multispectral_mode_and_t2_template` / `_with_a_legacy_record_are_kept` / `_without_any_record_are_kept`, `driver_tests::approved_normalize_reruns_when_the_method_changes`.

## 6. FSL FNIRT (contract)

- **FLIRT** pre-align moving→template; keep affines; delete flirt-reslice intermediate.  
- **FNIRT** with optional **ref/in brain masks** if skull-stripped pref.  
- Move `WarpField` / `InverseWarpField` to `forward_base+fnirt`, `inverse_base+fnirt` with correct extension.  
- **Apply** uses FSL `applywarp` / `fsl` branch — **not** `antsApplyTransforms`.

---

## 7. Schönecker 3-step affine (contract)

**Five** concatenated stages in one `antsRegistration` call: Rigid, Affine, Affine+mask1, Affine+mask2 with template masks **secondstepmask** / **thirdstepmask** on **fixed** side. Output **0GenericAffine.mat** + **inverse** mat via `antsApplyTransforms` math-only. File rename to `ants.mat` on forward and inverse **bases**. **Apply** uses **.mat** suffix in volume warp (not nii).

---

## 8. SPM paths (high level)

- **New seg:** run unified segment on all preop coreg channels → convert **y_** forward SPM warp to **ITK** nii.gz; build inverse (reference: slicer-invert or equivalent).  
- **DARTEL / SHOOT:** build segmentations → run Dartel/Shoot to template **dartel/shoot** nii in `template_space` — **0.5 mm** target in reference; **very slow** — expose timeout in Prefs.  
- **SPM apply branch:** if deformation not ITK, call **`convert_spm_warps(subject)`** before `ants_apply_transforms` (order matters — see integration tests).

---

## 9. EasyReg / SynthMorph (contract)

- **Inputs:** `template_t1` path, `anchor_coreg` path.  
- **Outputs:** two files → rename to `forward_base+ants.nii.gz` and `inverse_base+ants.nii.gz` (same naming as ANTs for **apply** layer).  
- **No** in-repo training weights in spec — **runtime path** in `Prefs` or env.

---

## 10. Re-apply only

Not implemented: there is no `apply` method (`--norm-method` accepts `ants_syn` only).

- **Input:** valid existing `norm.log.method` and transform files on disk.  
- **Action:** `apply_normalization` only — no new `antsRegistration`.

---

## 11. Apply warps to volumes

**`apply_normalization`:**

- Load `norm.log.method` JSON.  
- If method ∈ {ANTs, EasyReg, SynthMorph, SPM} → **ants_apply_transforms** branch (ITK).  
- If FNIRT → **fsl_apply_normalization**.  
- **Ref image:** **template** for forward, **anchor coreg** for inverse.  
- **Transform path:** `forward_base + suffix` where suffix = `ants.nii.gz` **or** `ants.mat` if method is affine-only (string contains `affine` / three-step).  
- **Interpolate:** high-quality for structural forward; `GenericLabel` for inverse (reference defaults).

**Post-op** volumes: if **brain shift** `scrf` exists, **bake** affine into a **temp** copy of post-op before warping (order: scrf then norm).

**Missing input file:** log skip line, do not fail whole batch (reference behaviour) — or **stricter** in Wyss DBS Localizer — **config**.

---

## 12. `apply_normalization_tofile` (volume IO)

- Same JSON branching; **`use_inverse`**; optional explicit **ref**; **SPM** path calls `convert_spm_warps` when needed.  
- Used by precoreg bb warp, export tools, `warpcoord` helpers.

---

## 13. Tonemap (post CT)

- Not implemented. Design text: after `normalize` returns, if `postop_modality==CT` → `tonemap_ct(phase=norm)`.

---

## 14. JSON method log (schema, minimal)

```json
{
  "method": "string, human-readable or machine token",
  "engine": "ants|wgpu",
  "approval": 0,
  "custom": false,
  "version": "optional"
}
```

**Must** be sufficient for `apply` and for **publication** methods paragraph (reference dumps prose elsewhere).

---

## 15. Edge cases

- **Schönecker** slab re-append in masked stages (reference algorithm).  
- **FNIRT vs ANTs** `use_inverse` in **point** mapping (documented in coordinate report — test bijectivity on sphere).  
- **Unreachable** elseif for SPM in reference apply — **Wyss DBS Localizer** **single** coherent branch.  
- **DARTEL** missing template → build or **E_DARTEL_TEMPLATE**.

---

## 16. Acceptance criteria

- [ ] Round-trip: native point → `forward` → `inverse` → native within **ε mm** (config, e.g. 0.5) on BIDS-like fixture.  
- [ ] `ants_syn`: produces valid nii.gz or mat + JSON.  
- Not implemented: other methods, `ants_scrf` on vs off (§5.1 lists it as part of the design), apply-only (§10).

---

## 17. References

- Avants 2008; Andersson 2010 (FNIRT); Ashburner 2005/2007/2011; Iglesias 2023; Hoffmann SynthMorph.  
- Schönecker 2009; Schoenecker 2008 masks (ANR **subcortical** refine).

---

## 18. Addendum — normalization checkreg (viewer + PNGs)

Batch and interactive **normalization** review share the same tool as coreg in reference. **Normative UI + `gencheckreg('norm')` detail:** `specs/03_coregistration.md` §12.1.2 and §12.3 (branches where `currvol` is `norm.anat.preop.(AnchorModality)`).

**Wyss DBS Localizer (Phase C):** `dbsloc run` executes normalization by default; use `--plan` for plan-only. With checkreg QC on by default (disable with `--no-checkreg` or `DBSLOC_OPEN_CHECKREG=0`), after successful SyN the pipeline runs the norm checkreg gate (`specs/03` §12.4a, `crates/dbsloc-pipeline/src/checkreg.rs` `run_norm_checkreg`): montage under `…/normalization/checkreg/`, manifest, optional `dbs-localizer checkreg`, and gates on `…/normalization/log/sub-*_desc-normmethod.json` (`approval` scalar). Pair = normalized anchor (`*_space-<Space>_desc-preproc_<mod>.nii.gz`) vs template (the isotropic-resampled template of §5.1 step 2 when it exists, i.e. unless `--norm-isotropic-mm full` was used); with multispectral SyN (§5.2) also the coregistered T2w vs the anchor (modality `T2w_to_<anchor>`). See `specs/08_addendum_interactive_volume_viewer.md`.

---

*End spec 04.*
