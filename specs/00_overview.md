# Spec 00 — Pipeline Orchestration & Configuration (Wyss DBS Localizer)

**Status:** Phase B (language-agnostic); §5.2, §6 and §6.1 are normative and describe the implemented behaviour; the other sections are design text unless they name the implementing code or say "not implemented".  
**Scope:** **Lead-DBS–class** single-subject pipeline. Lead Group and connectomes are in scope since 2026-10-23 (`14`, `15`); Lead-Connectome tractography and Lead-OR are out of scope.  
**Supersedes:** nothing.

---

## 1. Purpose

Define **orchestration** (ordered stages, dependencies, re-run rules) and the **configuration model** (options, user/machine preferences, per-dataset and per-subject data) for Wyss DBS Localizer. Other specs (`01`–`08`) define module behaviour; this document binds them.

---

## 2. Definitions


| Term                                             | Meaning                                                                                                                                            |
| ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Run**                                          | One execution of the pipeline for one subject (or batch of subjects).                                                                              |
| **Stage**                                        | Coarse step: preprocessing, precoreg, coregistration, normalization, brain-shift correction, reconstruction, orientation, VAT, visualization.      |
| **Anchor modality**                              | Primary pre-op MR contrast used as **spatial reference** (e.g. T1w if present, else next in order).                                                |
| **Native / anchor space**                        | 3D mm space of the **coregistered** anchor NIfTI (all pre-op and post-op volumes aligned to this frame after coreg). BIDS: `space-anchorNative`.   |
| **Template space**                               | Space of the **normalized** (MNI-like) images; name comes from the selected template package (e.g. MNI152NLin2009bAsym), not the same as “anchor”. |
| **Lock / approval**                              | On-disk **JSON** (or equivalent) that marks a stage’s outputs **approved**; orchestrator may **skip** recompute until inputs invalidate.           |
| **Wyss DBS Localizer `Options`**, `**Prefs**`, `**Subject**` | Canonical names for the Lead-DBS `options` / `prefs` / `subj` concepts (see §4).                                                                   |


---

## 3. Configuration layers

Config merges **top-down**; lower layers override higher where specified.

Implemented as: code defaults (`RunOptions::new`), then environment variables (`RunOptions::apply_env`, §5.2), then explicit CLI flags or `RunOptions` JSON keys. There is no `defaults.toml` and no machine-wide `Prefs` file; `uiprefs.mat` is read only per subject (electrode model). The layer list below is the original design.

1. **Project defaults** — versioned defaults in code.
2. **Machine / user preferences** — paths to external tools, thread counts, last-used method names, default template **space** name.
3. **Dataset** — BIDS `dataset_description.json` + template space metadata after dataset attach.
4. **Per-subject** — files for overrides, approval flags, recon method.
5. **Run** — `Options` for this invocation (subject id, which stages to run, overwrite flags).

**Norm:** All paths **absolute** in memory; config files may use paths relative to dataset root, resolved at load time.

---

## 4. Core types (abstract schema)

This section is **schema**, not a programming language. Field names are **suggested** Rust/Python identifiers; **§7** in `01_bids` lists path-level fields in full.

### 4.1 `Subject` (per-subject BIDS view)

- **Required:** `subj_id` (no `sub-` prefix in value), `subj_dir` (derivatives root for subject), `anchor_modality` (key string), `postop_modality` ∈ {`MRI`, `CT`, `None`}, paths for `preop_anat[mod]`: `raw` / `preproc` / `coreg` / (anchor) `norm`, `coreg` paths for post-op, `norm` log and transform **base names** (see `01_bids` + the Phase A archive (removed 2026-10-23, in git history) section `specs/phase_a_coordinate_spaces_report.md`).
- **Optional:** `brainshift` paths, `reconstruction` file path, `stim_dir`, `stats` path, `acpc` dir.
- **Derivation rules:** `anchor_modality` = first pre-op modality in a fixed priority list (T1w, T2w, …) when multiple exist.  
**Full layout:** `specs/01_bids.md`.

### 4.2 `Prefs` (user + machine)

- **Identity:** nifti extension default (`.nii` vs `.nii.gz`), pre-op anat order list, dicom stack behaviour.
- **Machine:** per-subtree settings for **ANTs** (`ants_preset`, `ants_scrf`, `ants_numcores`, …), **FSL**, **normalization** defaults, **VAT** `vatsettings` (per-model thresholds), **reconstruction** default method, **coregistration** default method, etc.  
Structure may mirror nested maps; exact keys in module specs (`03`–`07`).

### 4.3 `Options` (single run)


| Group             | Content                                                                                                                                                                             |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Identity**      | `earoot` (toolbox root — Wyss DBS Localizer install root), `dataset_root`, `patient_name` (`sub-xxx`), `subject_index` (batch), `cli_invocation` (bool)                                         |
| **Product**       | `lead_prod` = `dbs` only for this spec; other values rejected or ignored.                                                                                                           |
| **Bids**          | `bids_handle` (dataset navigator), embedded `spacedef` (template list, `norm_mapping`, `misfit_template`, `has_fa`, …)                                                              |
| **Subject**       | full `Subject` (see above) after resolution                                                                                                                                         |
| **Stage toggles** | Original design: `normalize_do`, `coreg_mr_do`, `coreg_ct_do`, `scrf_do`, `acpc_do`, `resize_do`, per-stage flags. Implemented: `RunOptions.stages` (`Vec<Stage>`, execution order), set by `dbsloc run --stages` or the `--no-<stage>` flags                                      |
| **Methods**       | `normalize_method`, `coreg_mri_method`, `coreg_ct_method`, `recon_method`, … (string enums per spec); Wyss DBS Localizer `RunOptions.norm_method` (`ants_syn`) and `RunOptions.reg_backend` (`auto` \| `wgpu` \| `ants`, serde default `auto`, the execution engine of `ants_syn`, `04` §5.4) |
| **Template**      | `primary_template` — filename **stem** of MNI (template-space) image matching anchor contrast (e.g. `t1`); from `spacedef.norm_mapping[strip_acq(anchor_mod)]` or `misfit_template` |
| **State**         | `native` (output coords: native vs template) — for writers                                                                                                                          |
| **GUI**           | absent in headless; optional handles in GUI mode only                                                                                                                               |


**Merge:** `Options` = defaults ⊕ environment variables ⊕ CLI flags or `RunOptions` JSON for this run (see §3).

### 4.4 Electrode and reconstruction handles

- `el_model` (string), `elspec` (resolved geometry) — `09_electrode_models.md`.
- `sides` — list of {left, right} to process.

---

## 5. Orchestration

### 5.1 Ordered stages (default `postop` present)

Original design pseudocode; it lists the Lead-DBS step order for reference only. The stage order that is implemented is in §5.2.

```
1. Init BIDS: resolve Subject, set primary_template, load spacedef
2. If DICOM import requested: import → rawdata; sync NIfTIs
3. For each preop modality (if preproc missing): copy raw → preproc; anat_preprocess; if anchor: maybe reslice_to_0.7mm
4. Precoreg anchor (if coreg nifti missing): spm coreg to template, write .mat, update header
5. If acpc/resize: run acpc
6. If coreg_mr: preop MRS to anchor; if postop MRI: postop MR to anchor
7. If postop CT and coreg_ct: CT to anchor; tone-mapped CT optional
8. If any coreg produced: checkreg (QC PNG)
9. If normalize: normalize (per method) → if CT postop: tonemap in norm
10. If scrf: brainshift GUI or headless branch → scrf .mat, derived images
11. Optional: warpdrive refine
12. If reconstruct: trac/core or pacer or manual
13. If orientation: DiODe
14. If vat: model dispatch
15. If visualize: 2D/3D write
```

**Idempotency:** Stages 3–8 typically skip if **output files exist and approved** and **input fingerprint** matches (lock JSON); `overwrite_approved` forces rerun.

**Parallelism:** Multi-subject = independent runs per subject (isolated `Subject`); intra-subject parallelism only where a module spec allows (e.g. ANTs threads).

### 5.2 Wyss DBS Localizer pipeline (normative for Wyss DBS Localizer)

**One implementation.** `crates/dbsloc-pipeline` is the pipeline; the GUI (`dbs-localizer`), the NDJSON runner (`dbsloc_pipeline_runner`, `RunOptions` JSON on stdin, `RunEvent` lines on stdout) and the native CLI (`dbsloc run`, the Rust binary `crates/dbsloc-cli`, calling `plan_run` / `run_pipeline_channel` in process) all drive it. Argument parsing, progress printing, the `uiprefs.mat` electrode read and the Lead-Tutor upload are Rust too (the Python CLI was removed 2026-10-12); Python is development tooling only (unification phase 2, 2026-10-05; Rust CLI 2026-10-12).

**Stages, in order:** `bids`, `preprocess`, `precoreg`, `coreg` (Rust label `coregister`; both names accepted), `normalize`, `brainshift`, `reconstruct`, `orientation`, `vat`, `visualize`. Lead Group jobs (`14` §4.6) and connectivity jobs (`15` §9.1) are not pipeline stages: they take no stage lock and reach subject stages only through the batch below (group *Calculate* runs `vat`, `07` §7.2) (added 2026-10-23; implemented, status per module in `AGENTS.md` §12). A run:

1. resolves the template (`04` §3.1) and the subject (`01` §10; failure: `dbsloc run` exits 2 before any stage; the runner fails each stage, except with `plan_only`, where stages report *plan*);
2. per stage, in order: if `locks/<stage>.json` approves it (§6) and `overwrite_approved` is off, the stage is *skipped* with result `skip:approved`; with `plan_only` every other stage is *skipped* with result `plan:<stage>`; otherwise the subject is re-resolved (so outputs of earlier stages are seen), the prerequisite check runs (message `<stage>: …`), then the body; after a successful `coreg` / `normalize` / `brainshift` the checkreg gate (`03` §12.4a) runs when enabled;
3. stops after a checkreg gate that is not approved (`StageAwaitingQc` + `StageFailed`, regardless of `stop_on_error`, 2026-10-14: dependents would consume the rejected registration), and after the first failure when `stop_on_error` (`dbsloc run` and `dbsloc_pipeline_runner` take it from the options or flags, else from `DBSLOC_STOP_ON_ERROR`, else from the value of `checkreg`; 2026-10-16), or when cancelled (Ctrl-C in `dbsloc run`, the GUI's stop button; the running tool or viewer is killed and the stage fails with `cancelled by user`, also when the tool's exit is what reports the interrupt).

**Environment overrides.** `dbsloc run` and `dbsloc_pipeline_runner` use the same variables to fill the run options that the options or flags leave unset (precedence: explicit option or flag, then environment variable, then default; `RunOptions::apply_env`, 2026-10-16). A field is explicit for `dbsloc run` when its flag is given, and for the runner when its key is present in the input JSON object. The variables are `DBSLOC_HEADLESS` (truthy → headless), `DBSLOC_OPEN_CHECKREG` (`0`/`false`/`no`/`off` → checkreg off; truthy → on), `DBSLOC_OPEN_SCENE3D` (off values → no scene viewer; truthy or `always` → viewer allowed), `DBSLOC_STOP_ON_ERROR` (on / off), `DBSLOC_REG_BACKEND` (`auto` / `wgpu` / `ants` → `RunOptions.reg_backend`, `04` §5.4), `DBSLOC_PREPROCESS_BACKEND`, `DBSLOC_COREGISTER_BACKEND`, `DBSLOC_NORMALIZE_BACKEND` (the per-step engines), `DBSLOC_NORM_ISOTROPIC_MM`, `DBSLOC_NORM_METHOD`, `DBSLOC_ANTS_MAX_THREADS`, `DBSLOC_VAT_BACKEND` (`auto` / `wgpu` / `cpu`, engine of the `vat` stage), `DBSLOC_VAT_LABELS` (stimulation labels to compute), `DBSLOC_VAT_STATS_ATLAS` (atlas set of the overlap statistics), and (2026-10-22) `DBSLOC_OUTPUT_DIR` → `output_dir`, `DBSLOC_POSTOP_MODALITY` → `postop_modality` (`auto` / `ct` / `mri`, `01` §10 step 5), `DBSLOC_LEADDBS_ATLASES_DIR` → `leaddbs_atlases_dir` (the external Lead-DBS atlases directory listed in the scene manifest, `11` §1; only `apply_env` reads it, the manifest writer takes the field). Unrecognised on / off values are ignored; an invalid engine, grid, method or thread count exits 2, but only when the field is not set explicitly. The GUI reads none of these variables; the server removes them from the runner's environment. The GPU adapter is chosen by `DBSLOC_GPU_BACKEND` (`auto|vulkan|dx12|metal`), `DBSLOC_GPU_ADAPTER` (name substring) and `DBSLOC_GPU_ALLOW_SOFTWARE` (`1` allows software adapters; hardware only otherwise). Viewers run with stdin and stdout on the null device (stdout of the runner carries only `RunEvent` lines).

**Results.** Each stage ends in exactly one event: `stage_completed {result: "ok:…"}`, `stage_skipped {reason, result: "plan:…" | "skip:…"}` or `stage_failed {message}` (`dbsloc run` prints `error: <message>`; any `error:` row makes it exit 1). Stage bodies without an implementation report *skipped*, never *completed*: `bids` → `plan:bids`, `brainshift` without `DBSLOC_SCRF_ANTS=1` → `plan:scrf:subprocess_not_implemented`, `vat` reports the tokens of `07` §7.4 (implemented, `crates/dbsloc-pipeline/src/vat/stage.rs`): `ok:vat:<computed>/<labels>` (with ` (native only)` when a label lacked the normalization warp), `skip:vat: no stimulation settings`, `skip:approved`, `plan:vat`, or a `stage_failed` message `vat: E_VAT_<code>: …` / `cancelled by user`. `precoreg` completes with `ok:precoreg:sub-<id>_<anchor>_precoreg` (`02` §6.1). `orientation` completes with `ok:orientation:<estimated>/<directional sides>` (`06` §10) or skips with `skip:orientation: no directional leads (<detail>)` (`06` §10.6) / `skip:orientation: post-op MRI not supported (specs/06)`. After a completed stage, and after a stage that stopped at an unapproved checkreg gate (its outputs are already in place), the invalidation of §6.1 runs and may print `dbsloc: <stage> re-ran; approvals invalidated: …` (shown even with `--log quiet`). `visualize` writes `reconstruction/viz/scene_manifest.json` (`ok:visualize:scene_manifest.json`) and launches nothing; the same manifest is written after every successful `reconstruct`, and `dbs-localizer scene` opens on it there, once per run, unless headless or `no_scene3d`. A missing display or the `dbs-localizer` program is a log note (`dbsloc: note: …`, printed even with `--log quiet`), never a failure. **Several subjects (2026-10-22, `specs/13` Part 2).** `dbsloc run <root> <id>…` (`--all-subjects`, `--subjects-file`) and the GUI queue run the subjects one after another through `dbsloc_pipeline::batch::run_batch` with one `RunOptions` template; each subject gets its own `plan_run` (a planning error is the outcome `plan_error`), its own event stream (`BatchEvent::Run(subject, event)`) and one outcome: `done`, `failed`, `awaiting_qc` (stopped at an unapproved checkreg gate under the deferred-review policy; never stops the batch), `plan_error`, `cancelled`. Policy `on_error` = continue (default) or stop (the remaining subjects are `cancelled` with the reason); a cancel marks the running subject and the rest `cancelled`; a panic of the pipeline thread is a `failed` outcome. Exit codes of `dbsloc run` with several subjects: 0 when no subject failed, 1 when any failed or was not found, 2 when every subject is unknown or on a usage error, 130 on Ctrl-C; the JSON summary becomes `{batch: true, subjects: […], totals: {subjects, done, failed, awaiting_qc, cancelled, plan_error, stages_ok, stages_failed, stages_skipped, seconds}, awaiting_qc: [ids], interrupted?}` while a single subject keeps the summary object of this section.

**Failure messages** (the `message` of `stage_failed`; `dbsloc run` prints `error: <message>`). Exact forms that tooling may match:

| Stage | Message |
| ----- | ------- |
| any | `cancelled by user`; `stage prerequisite missing: <stage>: …`; `BIDS error: …`; `I/O error: cannot replace <path>: …` |
| `preprocess` | `no anchor raw`; `anchor raw file missing`; `N4 failed with <code>` |
| `precoreg` | `no anchor anat`; `precoreg: no normalization template …`; `precoreg: antsRegistration not found (bundled or PATH)`; `precoreg: E_REG_BACKEND: …`; `precoreg: written image does not load: …` |
| `coreg` | `no postop CT or MR`; `no anchor anat`; `coreg: warped <key> is empty …` (post-op MR, `03` §4 step 3); `antsRegistration failed with <code>`; `coreg: …` (checkreg gate, `03` §12.4a) |
| `normalize` | `norm isotropic resample size must be > 0`; `no anchor moving image`; `no coregistered CT (*_space-anchorNative_desc-preproc_CT.nii.gz); run coreg or check CT/anchor`; `normalize: T2 template <path> does not exist or is not a file; …` (`04` §5.2); `need 3D (or 4D with last dim 1) for normalize`; `antsRegistration failed with <code>`; `normalize: T2w coregistration wrote no <name>`; `missing SyN composite <path> (antsRegistration output)`; `normalize: E_NORM_METHOD: unknown normalization method …` (`04` §4); `normalize: E_REG_BACKEND: …`, `normalize: <what> failed on <adapter>: …` (`04` §5.4); `norm: …` (checkreg gate) |
| `brainshift` | `scrf: DBSLOC_SCRF_ANTS=1 requires …` (the Python pipeline printed `error:scrf:` without the space); `antsApplyTransforms: binary not found (bundled or PATH)`; `antsApplyTransforms: missing <path>`; `antsApplyTransforms: moving or reference file missing`; `antsApplyTransforms (code <n>): <last 1500 characters of stderr>`; `brainshift: …` (checkreg gate) |
| `orientation` | `orientation: missing Reco JSON: <path>`; `orientation: missing raw CT: <path>`; `orientation: missing coreg transform: <path>`; `orientation: cannot re-warp the mni markers: …`; `orientation: cannot read …` |
| `reconstruct` | `recon:<message>` for every failure of the body, e.g. `recon:E_RECON: …`, `recon:E_PACER_mri: PaCER is CT-only (spec 05).`, `recon:E_RECON_elmodel: …` |

`reconstruct` with `recon_method` `pacer` or `trac_core_native` and a post-op modality other than CT fails with `recon:E_RECON: recon_method=<m> requires post-op CT (spec 05); …`, except `pacer` with modality `MRI`, which fails with `recon:E_PACER_mri: …`. Modality comparisons ignore case.

**Outputs are staged, then replaced.** A stage that runs an external tool (N4, `antsRegistration`, `antsApplyTransforms`) points the tool at short `.partial-*` work names in the target directory (external tools never receive the long `01` §7a names, so the Windows 260-character path limit is not hit) and, only when the stage body succeeds, renames each to its `01` §7a target (a target the run did not produce, e.g. a stale inverse composite or a post-op image of a key that no longer exists, is removed at that point). Options are validated before any of this. The staged set is the whole stage: `coreg` stages the CT outputs, the outputs of every post-op MR key and the coreg method log together, so a failure in one image keeps every previous image (`03` §4 step 3). On failure the partial files are removed and the previous outputs, and the stage's lock, stay as they were; if a rename fails after an earlier target was already replaced, the stage's lock is removed (the outputs are no longer the approved ones). Files the pipeline writes itself (Reco JSON, manifests, locks, checkreg PNG/JSON, the multistart record, the method logs) are written to `.partial-<name>` and renamed. Renaming creates a new file, so a derivative tree that hard-links another dataset's files never modifies the source through the link. Subject resolution ignores `.partial-*` files.

---

## 6. Lock / approval model

- **Input:** path to the **artefact** a stage is about to (re)write; optional **stale** link to upstream (e.g. preproc changed but norm approved).  
- **Output:** skip or recompute. The Lead-DBS design also had a float skip value and a `partial_invalidate` result (coreg lock 0.5); neither is implemented, the dependents are invalidated as in §6.1.
- **On-disk format:** JSON only; the exact format is given below. Minimum fields: `approval` (bool or numeric), `method` (string or structured), `timestamp` optional.  
- **User story:** “Approve” after QC; pipeline respects until upstream invalidation or until the stage itself runs again (§6.1 *The stage's own lock*).

**Acceptance:** Unit tests: lock file present + approved → stage skipped; missing → runs; `overwrite_approved` → always runs; upstream coreg file newer than norm approval → norm reruns (policy in `04`).

**Wyss DBS Localizer on-disk format** (`crates/dbsloc-pipeline/src/locks.rs`): one file per stage, `derivatives/leaddbs/sub-<id>/locks/<stage>.json` with the CLI stage name (`coreg.json`, not `coregister.json`). Fields, in this order: `approval` (bool), optional `method` (string), optional `inputs`, `timestamp` (seconds since the epoch, float); written as 2-space-indented JSON with a trailing newline (`dbsloc lock approve`). A stage is approved when `approval` (or legacy `approved`) is `true` or a non-zero number. `inputs` is a list of `{path, exists, mtime_ns, size, sha256}` per input file (`path` absolute and canonical; `mtime_ns`, `size`, `sha256` null when the file is missing). When the run fingerprints locks (`--fingerprint-locks`), the inputs per stage are: `bids` / `preprocess` / `precoreg` / `brainshift` → raw anchor; `normalize` → anchor + the raw non-anchor pre-op T2w when the subject has one (the second SyN channel, `04` §5.2; listed whether or not `norm_multispectral` is on, so a T2w change always re-runs normalize); `coreg` → raw anchor + every raw post-op image; `reconstruct` → the normalized post-op NIfTIs; `visualize` → the first of those + the Reco JSON; `vat` → per `07` §7.5: the Reco JSON, every canonical stimulation JSON computed, the forward and inverse normalization composites (the grey-matter mask is recorded only in the sidecar `inputs`); `orientation` → the first + normalized CT + raw post-op CT + the CT coregistration transform + Reco JSON (deduplicated). `coreg` and `normalize` fingerprint the anchor the stage actually reads: the N4-corrected anchor when it exists, else the raw one. An approved stage is skipped only while the stored entries and a fresh snapshot list the same set of paths (no duplicates) and each stored entry matches the fresh entry for the same path field for field (existence, size and `mtime_ns` are compared first and a file is hashed only when they match; a hash is computed at most once per path, size and mtime within a run — the result is the same as comparing every field); order is ignored, so lock files written by the former Python pipeline, whose order differed, still match. Paths compare case-insensitively on Windows. An entry without `sha256` (pre-hashing) never matches. An input that exists but cannot be read makes `dbsloc lock approve` fail and makes the check report *not approved*. A lock file that is not valid JSON is an error: `dbsloc lock show` raises it; a run prints `dbsloc: warning: corrupt lock <path>: …; the stage runs` and runs the stage. **`normalize` also checks what the outputs were made with:** an approved, otherwise skippable `normalize` runs when the channel record on disk shows a `method` other than `ants_syn`, another registration grid (`norm_isotropic_mm`), another template or T2 template, or another channel mode (T1w only vs T1w + T2w); it also runs when only another template's record exists (`04` §5.5; a missing record counts as `ants_syn`, and a record without the grid, template or channel keys, made before they existed, keeps its approved outputs for that field), with or without `--fingerprint-locks`.

**Per-side approval of `reconstruct` and `orientation` (2026-10-16).** These two locks are per subject and stage like every other lock, but a manual review of the leads (`05` §9a) is per side. The manual-correction writer approves `locks/reconstruct.json` (`method: "manual_correction"`) only when every lead in the file has been corrected or explicitly accepted, and `locks/orientation.json` only when every directed lead has its roll set or accepted; until then the stage stays unapproved, the per-side state lives in the Reco file (`meta.manual_correction[<k>]`), and a run of the stage keeps the reviewed sides and redoes only the others (`05` §9a *Reconstruct re-run*, `06` §10.6). `reconstruct` itself writes its lock approved after a run in which every side in the file was kept from a review. `overwrite_approved` drops the reviews (`05` §9a *Rerun all selected steps*).

### 6.1 Invalidation graph (Wyss DBS Localizer, normative)

A stage that **completes** (`stage_completed`, result `ok:…`) has replaced outputs that later stages read, so their approvals no longer describe what is on disk. A stage that committed its outputs and then stopped at a checkreg gate that is not approved (`stage_awaiting_qc` followed by `stage_failed`, §5.2 step 3) has replaced them too and invalidates its dependents in the same way, before the run stops. Skipped (`skip:…`, `plan:…`) stages invalidate nothing, except a stage body that replaced outputs and then returned a skip, which invalidates its dependents like a failure after replaced outputs (*The stage's own lock* below, case (b)); other failed stages invalidate nothing unless the failed attempt replaced outputs (*Replaced outputs and dependents after a failed attempt* below).

**Direct dependents** (what reads the stage's outputs):

| Stage that completed | Direct dependents |
| -------------------- | ----------------- |
| `preprocess` | `precoreg`, `coreg`, `normalize` |
| `precoreg` | `coreg`, `normalize` |
| `coreg` | `normalize`, `brainshift`, `reconstruct` |
| `normalize` | `brainshift`, `reconstruct` |
| `brainshift` | `reconstruct` |
| `reconstruct` | `orientation`, `vat`, `visualize` |
| `orientation` | `vat`, `visualize` |
| `vat` | `visualize` |

`bids` and `visualize` have no dependents. The set to invalidate is the **transitive closure** of the completed stage's dependents.

**Action per invalidated stage**, applied right after the completing stage's outputs are renamed into place and before the next stage starts (so a later stage of the same run is no longer skipped as approved):

1. **Stage lock** `locks/<stage>.json`: if it exists and approves, it is rewritten through `.partial-*` + rename with `approval: false`, the other fields kept, plus `invalidated_by` (the completed stage's CLI name) and `invalidated_at` (seconds since the epoch, float). A lock that does not approve, or does not exist, is left as it is. A corrupt lock is left as it is with the usual warning.
2. **Method-log approval** (the checkreg QC gate of `03` §12.4a): for an invalidated `coreg`, `normalize` or `brainshift`, an approval recorded in that stage's method log is cleared in the same way the viewer's *disapprove* clears it, so the QC gate asks again.
3. **Log:** one line per completing stage that invalidated anything, `dbsloc: <stage> re-ran; approvals invalidated: <stage>, <stage>` (dependents in pipeline order; only dependents whose lock or method log was approved), with `; reviews reset: <stage>, …` appended for dependents whose method log only carried a reviewer decision (`03` §12.4a step 5, e.g. a rejection), or `dbsloc: <stage> re-ran; reviews reset: …` alone; printed even with `--log quiet`.

**The stage's own lock (2026-11-12).** A stage that runs again replaces the outputs its own approval described, so that approval is withdrawn **before** the stage body starts. When `preprocess`, `precoreg`, `coreg`, `normalize` or `brainshift` is about to run (not skipped as approved, not `plan_only`) and its lock approves, the lock is rewritten as in step 1 with `invalidated_by` = its own CLI name and the run prints `dbsloc: <stage> runs again; its approval is withdrawn until the new result is approved`. If the approved lock cannot be withdrawn (it cannot be read or rewritten), the stage fails with `<stage>: cannot withdraw its approval before it runs again: <cause>` and its body does not run. The withdrawal stands after a stop at the gate and after any failure, cancel or killed process once the attempt has replaced an output; the original lock is written back in three cases: (a) a `coreg`, `normalize` or `brainshift` that completes with the checkreg gate on and whose method log was rewritten during this attempt and now approves (for `coreg`, every modality) — its new outputs were approved in the review of this run, printed as `dbsloc: note: <stage>: the new result was approved in its review during this run; its approval is kept`. Each run writes a fresh method log with `approval` 0, so within one run only that review sets it; a second writer of the same method log while the stage runs (a server slide approval without an awaiting job, a viewer left open from an earlier run) can also set it and is not told apart. With `--fingerprint-locks` the written-back lock carries the input fingerprints of this run (`…; its approval is kept, with the input fingerprints of this run`), otherwise its bytes are unchanged; (b) a stage body that returns a skip without replacing an output (e.g. the `brainshift` stub); a body that replaced outputs and then returns a skip is treated like a failure after replaced outputs: the approval stays withdrawn and the dependents are invalidated; (c) a failure or cancel before the attempt replaced any output, e.g. a missing tool or template, a registration that fails, a cancel during the registration (`dbsloc: note: <stage>: the step stopped before it replaced any output; its approval is kept`). A lock is written back only while it still reads `approval: false` with `invalidated_by` = the stage; a lock someone else wrote during the run is left as it is, with a note. This covers every reason a stage runs although its lock approved it: `overwrite_approved`, a changed input fingerprint, or a record that no longer matches the options (another template, grid, method or registration backend, §6). `reconstruct`, `orientation` and `vat` manage their own lock (§6 *Per-side approval*, `07` §7.5); `bids` and `visualize` are not affected. A missing lock stays missing.

**Replaced outputs and dependents after a failed attempt (2026-11-12).** For these five stages the run watches, from the withdrawal until the stage ends, every output the staged commit or the atomic file writer (`outputs::Staged::commit`, `outputs::write_replacing`) replaces or removes under the subject's output folder (and, for `brainshift`, at `DBSLOC_SCRF_OUTPUT`; writes to the `.partial-` temporary files of a staged commit, or inside a `.partial-` folder, do not count, the commit that renames them does), including a commit that fails part-way after its first target; it also compares the size and modification time of the stage's own files before and after (the method log for `coreg`, `normalize`, `brainshift`; the output image and record for `preprocess` and `precoreg`). Either signal means the attempt replaced outputs. A failure or cancel other than the gate stop (for example *Cancel* during the GUI review, `checkreg needs a display`, a slide that cannot be drawn, a failed rename) after replaced outputs invalidates the dependents exactly as a completed stage does; without replaced outputs it invalidates none and writes the own approval back (case (c) above). Writes of another run in the same process under the same subject folder also count, which only errs towards invalidating. Writes by another process (a second `dbsloc`, the server's runner, the GUI) to the same subject are not seen by the watch; only the size and modification-time probe of the stage's own files can catch them. A killed process cannot do this step; its own lock is already withdrawn, and the dependents are invalidated when the stage next completes.

**Continuing after a stop at the gate.** Approve in the viewer or the GUI review: the GUI and the server then run only the stages after the gate. On the command line, a later `dbsloc run` runs the stage again unless its lock approves: run `dbsloc lock approve <dataset> <subject> <stage>` and then `dbsloc run`, which skips the stage. With `--fingerprint-locks` the lock needs its inputs: pass each path listed under `lock_input_paths.<stage>` in the JSON of `dbsloc run <dataset> <subject> --plan --fingerprint-locks` as `--input <path>`; an approval without inputs never skips a fingerprinted stage.

Outputs of invalidated stages are **not** deleted: they stay until the stage re-runs (the staged-replace rule of §5.2 removes stale targets then). `overwrite_approved` does not change this section. `plan_only` runs complete nothing and therefore invalidate nothing.

**Acceptance:** approve `normalize` and `reconstruct`, run `coreg` → both locks read `approval: false` with `invalidated_by: "coreg"`; a run `--stages coreg,normalize` re-runs `normalize` although its lock was approved before the run; a skipped `coreg` (approved) changes no other lock and keeps its own byte for byte. Approve `normalize`, run it with `overwrite_approved` and stop at its gate → `locks/normalize.json` reads `approval: false` with `invalidated_by: "normalize"`, and the next run without `overwrite_approved` runs `normalize` instead of skipping it (same for `coreg` and `brainshift`); a completed re-run of `preprocess` or `precoreg` withdraws its approved lock the same way, as does a `normalize` that completes with the gate on but an unapproved method log; a `normalize` or `brainshift` re-run approved in its review during the run keeps its lock byte for byte; a review cancelled after `normalize` rewrote its method log, and a commit that fails after replacing its first target, leave `normalize` withdrawn and `reconstruct` invalidated by `normalize`; a failure before the commit (also a missing prerequisite) leaves both locks byte for byte; a lock that cannot be rewritten stops `normalize` before its body; a `coreg` log with one modality still 0, or an approving log not rewritten in the attempt, does not write the approval back; a lock rewritten by someone else during the run is not overwritten.

---

## 7. Coordinate and transform contract (summary)

- **One** subject has: coreg **linear** transforms (per modality), norm **nonlinear (or affine-only)** forward/inverse **composite** on disk, optional **scrf** linear on post-op.  
- **Point mapping:** from native voxel ↔ template mm uses **chained** transforms in order defined in `04` (`forwardTransform` / `inverseTransform` path convention in `01`).
- **Full detail:** `specs/01_bids.md` (paths) + `04_normalization.md`.

---

## 8. External programs (orchestrator view)


| Stage                            | External                                           |
| -------------------------------- | -------------------------------------------------- |
| Image import                     | dcm2niix (bundled) `01` §8.4                       |
| Preproc                          | ANTs N4 or the in-process wgpu N4 `02`             |
| Precoreg, coreg                  | ANTs or the in-process wgpu engine `03`            |
| Normalization                    | ANTs or the in-process wgpu engine `04`            |
| Recon, VAT, viz                  | `05`–`08` (in process; VAT may use wgpu)           |

The original design also listed SPM, FSL, BRAINSFit, EasyReg and SynthMorph; none of them is wired in.

Orchestrator **invokes** external tools via subprocess with logged argv; no embedded MATLAB.

---

## 9. Error handling (global)

- **Fatal:** subject dir missing, no anchor modality, BIDS parse failure, tool binary missing, norm method unknown.  
- **Recoverable:** single modality missing (skip with log), DICOM slice gap (import warning).  
- **User:** approval required to proceed after QC failure (policy in §5.2 step 3 and `03` §12.4a).

---

## 10. Acceptance criteria (orchestrator)

- Given valid `Options` + `Subject`, system computes **topological** stage order: no norm before coreg for moving images, etc.  
- Lock files honored per §6.  
- `primary_template` matches the template contrast chosen for the anchor (`04` §3.1).  
- **Batch:** N subjects, each with isolated output tree.  
- **CLI** entrypoint documents stage flags (a config file round-trip is not implemented; `RunOptions` JSON is the machine-readable form).

---

## 11. References

- Lead-DBS v2: Horn et al. (2019) *NeuroImage* 184, 293–316 (pipeline overview; not algorithm source).  
- BIDS: BIDS 1.x + derivatives conventions (BEP).  
- User-facing: [https://netstim.gitbook.io/leaddbs](https://netstim.gitbook.io/leaddbs) (workflow descriptions).

---

## 12. Mapping (legacy import only)


| Lead-DBS             | Wyss DBS Localizer (this spec)                      | Implementation (`crates/dbsloc-pipeline/src/`)         |
| -------------------- | --------------------------------------- | ----------------------------------------------------- |
| `options`            | `Options` + embedded `Subject` + `Bids` | `options.rs` `RunOptions`                             |
| `ea_prefs` / `prefs` | `Prefs`                                 | `bids.rs` (prefs index)                               |
| `subj` (BIDSFetcher) | `Subject`                               | `bids.rs` `Subject`, `resolve_subject_with`           |
| `ea_autocoord`       | orchestration procedure §5              | `lib.rs` `run_pipeline_channel`, `stages.rs`, `prereqs.rs`, `stage_impl.rs` |
| `ea_reglocked`       | §6 lock evaluation                      | `locks.rs`                                            |


*End of spec 00.*