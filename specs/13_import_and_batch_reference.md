# 13 — Reference behaviour: image import, BIDS conversion and batch processing

> Phase B behaviour reference written 2026-10-22 from observation of the reference implementation (no reference code). Normative Wyss behaviour stays in `specs/01` §8.4 (import) and `specs/00` §5 (runs); this document records what the reference does so that Phase C can close or deliberately keep each gap (§14). Referenced as `specs/13` §n.


**Status:** Phase B (behaviour description, language-neutral).
**Reference observed:** Lead-DBS working tree at commit `5b1008d` (2026-03-02), read-only. Nothing below is code; it describes what the reference application does, so that Phase C can close gaps against `specs/01_bids.md` §8.4 and `specs/00`.
**Conventions:** "the reference" = Lead-DBS. Quirks that look like bugs are marked **(quirk)**; Phase C need not copy them. Points not fully verified are marked **(uncertain)**.

---

## PART 1 — Image import and BIDS conversion

### 1. Overview of the reference import model

The reference has one dataset model: a BIDS-like root with `rawdata/`, `sourcedata/`, `derivatives/leaddbs/`. Import is a two-phase process:

1. **Staging** — the user hands over a folder (or files). The reference copies it, unchanged, into a staging place for one subject: DICOM → `sourcedata/sub-<id>/DICOM/…`; NIfTI → `rawdata/sub-<id>/unsorted/`.
2. **Classification** — for each staged subject, DICOM is converted to NIfTI into `rawdata/sub-<id>/unsorted/`, and a **classification window** (one per subject) shows every unsorted NIfTI in a table. The user assigns session / type / modality / acquisition and ticks *Include*. On OK the included files are **copied** to their BIDS names and the subject index `rawimages.json` is regenerated from the directory contents.

No pipeline step (preprocess, coregistration, …) runs in the same run as an import (see §2.5).

### 2. Entry points and order of steps

#### 2.1 Dataset root (asked first, or lazily)

- Main window shows a button *Choose Dataset Directory* plus a *Recent datasets* drop-down.
- Folder dialog starts at the parent of the most recently used dataset, else at `<home>/Documents/LeadDBSDataset` (created on the fly; deleted again if the user picks another folder).
- If the user picks `…/derivatives` (containing `leaddbs`) or `…/derivatives/leaddbs`, the root is walked up to the dataset root.
- **Dataset folder name rule:** the last path component must be alphanumeric only (no `_`, `-`, spaces); otherwise a hard error "Should only contain alphanumeric characters".
- On selection the reference creates `derivatives/leaddbs/`, `rawdata/`, `sourcedata/` and, when missing, `dataset_description.json` and `.bidsignore` (§4.10).
- If the user adds a patient folder (§2.2) before a dataset is chosen, the dataset dialog appears **then** (input first, dataset second). There is no separate "output" step after that.

#### 2.2 Adding patients ("+" button, drag and drop, or command line)

The *Add patient* button opens a multi-select folder dialog ("Please choose patient folder(s)..."); dropping folders on the window does the same. Each selected folder is classified by the **first** folder of the selection (the whole selection is assumed homogeneous):

| Test (in order) | Detected kind | Action |
|---|---|---|
| Folder contains `ea_ui.mat` or `ea_reconstruction.mat` | Legacy Lead-DBS patient folder | Ensure a dataset root (§2.1), run legacy migration (§2.8) for every selected folder |
| Path ends in `<root>/(derivatives/leaddbs|rawdata|sourcedata)/sub-<id>` | BIDS subject folder | Load that dataset; if it is a *different* dataset from the loaded one, ask: *Copy the selected patients to the current dataset* / *Switch to the selected dataset* / *Cancel*. On copy, existing ids ask *Skip* / *Overwrite* / *Cancel* (overwrite deletes the subject's derivatives, rawdata and sourcedata first) |
| Folder is a dataset root (has `derivatives`, `rawdata` or `sourcedata`) or one of those subfolders | BIDS dataset | Load the dataset, select its first subject |
| Otherwise, per selected folder: folder (top level only) contains `*.nii` / `*.nii.gz` | Loose NIfTI folder | Copy the whole folder to `rawdata/sub-<id>/unsorted/` |
| Otherwise: `dcm2niix` query mode reports ≥ 1 DICOM file below the folder | DICOM folder | Copy to `sourcedata/sub-<id>/DICOM/` (if the folder itself is named `dicom`, case-insensitive, its *contents* are copied) |
| Otherwise | — | Console warning "Incompatible folder", skipped |

**Subject id (never asked):** the selected folder's own name with every non-alphanumeric character and `_` removed, with a warning when anything was removed. (The intent to use the parent folder name when the folder is called `DICOM` is defeated by a full-path comparison, so a folder named `DICOM` yields subject `DICOM` **(quirk)**.) The user can rename later (*Rename patient*; new id must be `[a-zA-Z0-9]`). Existing subject folders of the same id are silently reused.

**Multi-subject import:** selecting several patient folders stages all of them in one go; each becomes one subject.

#### 2.3 Patient list state after staging

After loading, the reference computes per subject four flags: *has index* (`prefs/sub-<id>_desc-rawimages.json` exists), *has rawdata* (any `ses-preop|ses-postop … .nii.gz`), *has unsorted rawdata*, *has sourcedata*. Subjects **without an index but with unsorted or source data** are highlighted (light red) in the patient list; the status line says "Unsorted NIfTI/DICOM folder found (highlighted), please run Import first." The *Import* tab is brought to front, all processing checkboxes are cleared, and the two import checkboxes are pre-set:

- *Import DICOM to BIDS dataset* — ticked if any highlighted subject has sourcedata but no unsorted NIfTI.
- *Import NIfTI to BIDS dataset* — ticked if any highlighted subject has unsorted NIfTI.
- *DICOM conversion tool* drop-down: `dcm2niix` (default), `dicm2nii`, `SPM`, `GDCM via Slicer`.
- *Regenerate rawimage.json* button: rebuilds the index of the selected subjects from their files (§4.9).

Subjects with rawdata but no index get their index generated automatically on dataset load.

#### 2.4 Running the import

The user presses *Run*. For each selected subject, in list order, sequentially:

1. If *Import DICOM* is ticked and the subject has sourcedata: convert everything under `sourcedata/sub-<id>/` to `rawdata/sub-<id>/unsorted/` (§3). No output → warning "No output found from DICOM to NIfTI conversion", subject ends.
2. If either import box is ticked: gather `unsorted/*.nii[.gz]`; if none, warning and subject ends; else open the classification window (§4), which **blocks** until closed.
3. Window result `okay`, `discard` or `empty` → delete `unsorted/`; `cancel` → keep it (subject stays highlighted).
4. **Return without running any pipeline stage**, even if registration / localization boxes are ticked. A second *Run* is needed for processing.

Because DICOM stays in `sourcedata/`, ticking *Import DICOM* again later reconverts everything and re-opens the window.

#### 2.5 Adding images to an existing subject

With exactly one subject selected, two extra buttons appear:

- **Add DICOMs**: folder dialog; the folder is copied to `sourcedata/sub-<id>/DICOM/<random-uuid>/` and converted immediately into `unsorted/`.
- **Add NIfTIs**: file dialog; files copied into `unsorted/`.

Then, in both cases: every existing `ses-preop` / `ses-postop` NIfTI + JSON is backed up (`<file>.bak`) and **moved into `unsorted/`**; the classification window opens with all files, the previously imported ones pre-ticked (*Include* = true) and pre-filled from their BIDS names (§4.2 rule A). Result `okay` → backups deleted, subject reloaded; `cancel` → backups moved back (Add DICOMs also deletes the new DICOM copy). `unsorted/` is deleted afterwards in every case. (On `discard`, the old files are neither restored nor re-imported; only the `.bak` copies remain **(quirk)**.)

- **Replace/delete NIfTIs** dialog: a tree of the subject's existing raw / preprocessed / coregistered / normalized images; *Replace File* swaps one file for a user-chosen one, *Delete File(s)* removes one or a whole modality group; the index is regenerated afterwards. Downstream results and approvals are **not** invalidated.

#### 2.6 Loose NIfTI files for a *new* subject

There is no "new subject" form. The user must put the files in a folder whose name is the intended id and add that folder (§2.2), then run the import (§2.4).

#### 2.7 Headless import (command line)

`lead import -dataset <root> -patientid <id> [-electrodemodel <name>] <file1> <file2> …` (aliases: `-bids`, `-patient`, `-id`, `-ele`, `-lead`, `-model`). No window; rules:

- Creates `derivatives/leaddbs/`, `rawdata/`, `sourcedata/`, `dataset_description.json` (§4.10, plain variant), `derivatives/leaddbs/sub-<id>/prefs/`, `rawdata/sub-<id>/ses-preop/anat/` and `ses-postop/anat/`.
- Classification by **case-insensitive substring of the full path** (directories included **(quirk)**), first match wins:

| Path contains | Result |
|---|---|
| `T1` | pre-op `T1w` |
| `T2` | pre-op `T2w` |
| `FLAIR` | pre-op `FLAIR` |
| `FGATIR` | pre-op `FGATIR` |
| `CT` (only checked if none of the above) | post-op `CT` |
| anything else | silently ignored |

- Pre-op files get `acq-<tag>` from the voxel rule (§4.5): `sub-<id>_ses-preop_acq-<tag>_<suffix>.nii.gz`. CT: `sub-<id>_ses-postop_CT.nii.gz`. No post-op MRI possible. No duplicate check (a second `T1` file overwrites the first if the acq tag matches). No sidecars copied.
- Index regenerated (§4.9); per-subject UI preferences file `prefs/sub-<id>_desc-uiprefs.mat` created from defaults, with the electrode model set when given.

#### 2.8 Legacy patient folder migration

Triggered automatically when a legacy folder is added (§2.2), always in "full" mode (rawdata **and** derivatives). Rules relevant to import:

- Subject id: folder name stripped of non-alphanumerics; a leading `sub` (any case) becomes `sub-`, else `sub-` is prepended.
- Files ignored: names starting `lpostop`, `lanat`, `ea_pm`, `rc`, `u_rc`, `j_rc`, `v_rc`, `y_anat_`, `iy_anat_`, `F.`, `DICOMDIR`, `Thumbs`, `Icon`; ending `.ps`; containing `tmp`, `temp`, `copy`, `grid`, `seg8`, `ignore`, `backup`; `.h5` other than `glanat*` / `*_brainsfit*`.
- Pre-op raw files: names starting `raw_anat_` (`.nii`). Post-op raw files: names starting `raw_postop_` or `postop_ct`.
- **Legacy name → BIDS modality lookup** (case-insensitive substring of the file name, first entry in this order that matches wins):

| Order | Legacy token | BIDS |
|---|---|---|
| 1 | `t1` | `T1w` |
| 2 | `t2star` | `T2starw` |
| 3 | `pd` | `PDw` |
| 4 | `ct` | `CT` |
| 5 | `tra` | `acq-ax_MRI` |
| 6 | `cor` | `acq-cor_MRI` |
| 7 | `sag` | `acq-sag_MRI` |
| 8 | `fgatir` | `FGATIR` |
| 9 | `fa` | `fa` |
| 10–12 | `dti`, `dti.bval`, `dti.bvec` | `dwi`, `dwi.bval`, `dwi.bvec` |
| 13 | `t2` | `T2w` |
| 14 | `flair` | `FLAIR` |
| 15 | `inv` | `INV` |
| 16 | `swi` | `SWI` |
| none | — | upper-cased remainder after `[raw_]anat_` |

- Outputs: pre-op `sub-<id>_ses-preop_acq-<tag>_<mod>.nii.gz` (tag from §4.5; a name clash appends a counter to the acq value: `acq-iso1`, `acq-iso2`, …); post-op `sub-<id>_ses-postop_<mod>.nii.gz` (e.g. `…_ses-postop_CT.nii.gz`, `…_ses-postop_acq-ax_MRI.nii.gz`); `dti.*` → `ses-preop/dwi/`. `DICOM/` → `sourcedata/sub-<id>/`. Derivatives are renamed per a fixed mapping (out of scope here). If no raw pre-op files exist but coregistered ones do, the coregistered images are copied back as raw and preprocessed images. A naming report `derivatives/leaddbs/logs/legacy2bids_naming.xlsx` (source → destination) is written.

### 3. DICOM conversion

#### 3.1 Tool and invocation

Default tool `dcm2niix` (bundled). Behaviour of the call:

| Aspect | Value |
|---|---|
| Output folder | `rawdata/sub-<id>/unsorted/` (created) |
| Input | whole `sourcedata/sub-<id>/` tree (GUI *Run*), or the newly copied DICOM folder (*Add DICOMs*) |
| Flags | progress reporting on; file-name pattern (below); **ignore derived, localizer and 2-D images** on; BIDS JSON sidecar on (default anonymisation); verbosity 0; gzip output on |
| File-name pattern | `<input folder name>_<protocol name>_<sequence name>_<date+time>_<series number>_<series description>` (dcm2niix tokens `%f_%p_%z_%t_%s_%d`) |
| DICOM detection | dcm2niix's directory-query mode ("found N DICOM files"); N ≥ 1 ⇒ DICOM folder |
| Success test | at least one `*.nii.gz` in the output folder |

Alternatives: `dicm2nii` (MATLAB converter, JSON sidecar on), SPM's DICOM import, and a 3D Slicer (GDCM) script. Preferences `prefs.migrate.*` and `prefs.dicom.*` exist but are **not read** anywhere (dead settings).

#### 3.2 Grouping and metadata

- **Series grouping is entirely the converter's** (one NIfTI per series / echo / reconstruction as dcm2niix splits them). The reference adds no grouping of its own, no study/session grouping, and no merging.
- Metadata the user sees, only for information (not used in logic): the file name tokens (protocol, sequence, date-time, series number, description); in the classification window's info panel: matrix size, voxel size, acquisition date (read from a dicm2nii NIfTI header extension only, so "N/A" for dcm2niix output **(quirk)**), intensity range, orientation code, and **every key of the JSON sidecar** (e.g. `SeriesDescription`, `ProtocolName`, `Modality`, `AcquisitionTime`, …).
- Metadata used in logic: only the **file name** (lookup rules, §4.2) and the **voxel sizes** (acq guess, §4.5), plus `PhaseEncodingDirection` for `bold` / `dwi` duplicates (§4.6). `Modality` (`CT`/`MR`), `SeriesNumber`, `ImageOrientationPatient` are **not** used.

#### 3.3 Dates, sessions, pre-op vs post-op

- Sessions are fixed: `preop` and `postop` (no `ses-<date>`; the prefs keys `ses-preDBS` / `ses-postDBS` exist but are not used by import).
- **The acquisition date is never used to decide anything** and is never offered as a choice between studies. All series of all studies in the DICOM tree land in one flat table.
- Pre-op vs post-op is decided by: (a) the lookup table — only the `CT` row is post-op by default; (b) otherwise the user. Every non-CT row defaults to pre-op if a pre-op keyword matched, else to "-" (unset). Post-op MRI is never guessed.

### 4. The classification window ("NIfTI to BIDS")

Title strings: "Conversion for subject: sub-<id>", "BIDS root: <root>". Layout: table (left), a live **preview tree** of the resulting BIDS subject (`sub-<id>` → `ses-preop`/`ses-postop` → `anat`/`func`/`dwi` → file names), three orthogonal slice views with a crosshair (click to navigate, scroll wheel moves ±2 slices), an intensity histogram (values between the median and the 95th percentile), and the info panel. Buttons: *OK*, *Cancel*, *Modify lookup table*. Before the window opens every `.nii` input is gzipped in place; unreadable images (e.g. 1-D) are dropped with a warning; if none remain the result is `empty`.

#### 4.1 Table columns

| Column | Editable | Allowed values | Notes |
|---|---|---|---|
| Include | yes | true / false | default false (or "pre-set" for re-presented files, §2.5) |
| Filename | no | — | stem without extension |
| Session | yes | `-`, `preop`, `postop` | |
| Run | yes | free text, `-` = none | |
| Task | yes | free text, `-` = none | |
| Type | yes | `-`, `anat`, `func`, `dwi` | |
| Modality | yes | `-`, `T1w`, `T2w`, `FGATIR`, `FLAIR`, `T2starw`, `PDw`, `WMn`, `bold`, `sbref`, `dwi`, `CT`, `MRI` | no `SWI`, no "ignore" value — ignoring = Include unticked |
| Acquisition | yes | `-`, `ax`, `cor`, `sag`, `iso` | not free text, although the help text says "unique identifier via the acquisition column" **(quirk)** |

#### 4.2 Pre-fill rules (applied once when the window opens, and again after the lookup table is edited)

**Rule A — file already BIDS-named** (a valid BIDS name with `sub-…`): take `ses` → Session, suffix → Modality, Type = `func` for `bold`, `dwi` for `dwi`, else `anat`; `run`, `task`, `acq` entities → their columns. Nothing else is applied to that row.

**Rule B — lookup table** (otherwise): strip a leading `sub-<id>_` from the file stem; for every (type, session, modality, keyword) of the lookup table, in table order, test the keyword as a **case-insensitive regular expression search** anywhere in the stem; **every** match overwrites Session, Type and Modality, so the **last matching entry wins**.

Default lookup table (shipped JSON, structure `type → session → modality → [keywords]`), in evaluation order:

| # | Type | Session | Modality | Keywords |
|---|---|---|---|---|
| 1 | anat | preop | T1w | `t1`, `mprage` |
| 2 | anat | preop | T2w | `t2`, `tse` |
| 3 | anat | preop | FGATIR | `FGATIR` |
| 4 | anat | preop | PDw | `pd` |
| 5 | anat | preop | T2starw | `t2star` |
| 6 | anat | preop | FLAIR | `flair` |
| 7 | anat | postop | CT | `Navigation`, `CT` |
| 8–10 | anat | postop | `ax_MR`, `sag_MR`, `cor_MR` | (empty) |

Consequences (by last-wins): `t2star…` → T2starw; `t2_flair` → FLAIR; anything containing `ct` anywhere (including the patient folder token and words like "direct", "sect") → post-op CT; post-op MR rows can never match (empty lists; their names are not valid Modality values either). Because the dcm2niix name starts with the input folder name, the folder name participates in matching **(quirk)**.

**Rule C — acquisition guess:** for every row whose Session is not `postop` and Type is not `func`/`dwi` (including unmatched rows), Acquisition := voxel rule (§4.5).

The *Modify lookup table* dialog edits rows (*Keywords* comma-separated, *Type*, *Session*, *Modality*), offers *Load defaults*, *Load .json*, *Save*, *Cancel*; the edited table is stored in the user's MATLAB preferences and applies to all later imports.

#### 4.3 Coupling rules when the user edits a cell

Applied to the edited row only (Filename/Include edits excluded):

| User action | Automatic change |
|---|---|
| Session → `preop` on an `anat` row with Modality `MRI` | Modality := `T1w`, alert to check it |
| Session → `postop` on an `anat` row whose Modality is not `CT` | Modality := `MRI` |
| Type → `anat` | Task := none, Modality := `T1w`, alert |
| Type → `func` | Task := `rest`, Modality := `bold` |
| Type → `dwi` | Task := none, Modality := `dwi` |
| Modality ∈ pre-op set (T1w … WMn) | Type := `anat`, Session := `preop`, Task := none |
| Modality ∈ {`CT`, `MRI`} | Type := `anat`, Session := `postop`, Task := none; for `MRI` with Acquisition not in {ax, cor, sag}: Acquisition := `ax` (alert if it was set) |
| Modality = `bold` | Type := `func`; Task := `rest` the first time only |
| Modality = `dwi` | Type := `dwi`, Task := none |
| Any edit leaving Session, Type, Modality all set on an un-included row | Include := true |

Then, for every included row: `CT` → Acquisition := `-`; any other modality with Acquisition `-` → voxel rule; for `MRI` a non-orientation result (`iso`) → `ax` with a warning.

#### 4.4 Output file names

`sub-<id>_ses-<session>[_run-<run>][_task-<task>][_acq-<acq>][_dir-<dir>]_<modality>` with `.nii.gz` + `.json` (+ `.bval`, `.bvec` for `dwi`), into `rawdata/sub-<id>/ses-<session>/<type>/`. Note the entity order run → task → acq, which differs from the BIDS standard order **(quirk)**. Examples: `sub-01_ses-preop_acq-iso_T1w.nii.gz`, `sub-01_ses-postop_CT.nii.gz`, `sub-01_ses-postop_acq-ax_MRI.nii.gz`. Files are **copied** from `unsorted/` (missing sidecar = warning only).

#### 4.5 Acquisition (orientation) guess from voxel sizes

Input: the three voxel sizes `(p1, p2, p3)` along the **voxel index axes** (the affine is not consulted).

1. If `max − min < 0.05 mm` → `iso`.
2. Else, if exactly two distinct values occur (exact equality): the axis holding the value that occurs once is the "slice axis".
3. Else (three distinct values): the slice axis is the one whose in-plane area (product of the *other* two sizes) is smallest.
4. Slice axis 1 → `sag`, 2 → `cor`, 3 → `ax`.

(Rule 2 picks the odd axis even when it is the *thin* one, e.g. `(2, 2, 0.5)` → `ax`.)

#### 4.6 Duplicates

- Duplicates are detected on the **generated file name**: two included rows producing the same name are both shown in the preview tree as `>> name <<`. *OK* then refuses: "Multiple files with same modality included … Please select only one file per modality and session or separate them by specifying a description."
- Resolution is manual: untick one, or change Acquisition (`iso`/`ax`/`cor`/`sag`) or set Run.
- `bold` and `dwi` only: if both rows' sidecars carry different `PhaseEncodingDirection`, a `dir-` entity is added automatically: `j-` → `AP`, `j` → `PA`, `i` → `LR`, `i-` → `RL`.
- There is no automatic `acq-` counter (only the legacy migrator does that, §2.8).

#### 4.7 OK-button validation (in this order; first failure shows an alert and keeps the window open)

1. No included pre-op `anat` file → "No preop files included. Please select at least one preop image."
2. No included post-op `anat` file → confirmation "No postop anatomical files included. Do you still want to proceed?" (*Yes* continues without post-op checks).
3. Any `>> … <<` duplicate → blocked (§4.6).
4. Any included row with Session, Type or Modality unset → "Please specify session, type and modality for all Included images."
5. Post-op rows exist but none has Modality `CT` or `MRI` → blocked, listing `CT, MRI`.
6. Post-op `MRI` rows: each must have Acquisition `ax`, `sag` or `cor`; **at least one must be `ax`** ("No postop MRI with the acquisition <ax> found …"). (Phase C deliberately relaxes this: any one orientation suffices, `specs/01` §8.1.)

Then: copy files (§4.4), regenerate the index (§4.9; a failure there is a non-fatal warning), close with result `okay`.

#### 4.8 Cancel

*Cancel* or closing the window asks: *Yes (keep unsorted files)* → result `cancel` (staging kept); *Yes (discard unsorted files)* → result `discard` (staging deleted by the caller); *No* → back to the table.

#### 4.9 `rawimages.json` (subject index)

- **Location:** `derivatives/leaddbs/sub-<id>/prefs/sub-<id>_desc-rawimages.json` (not under `rawdata/`).
- **Built from the directory, not from the table:** every `*.nii[.gz]` below `rawdata/sub-<id>/` whose path contains `/ses-preop/` or `/ses-postop/` (case-insensitive), pre-op first. For each file: session = lower-cased `ses` entity; datatype = parent folder name (`anat`, `func`, `dwi`); key = `<acq>_<suffix>` if an `acq` entity exists, else `<suffix>`; value = file name without extension. Later files with the same key overwrite earlier ones (e.g. two `run-` files of the same acq/suffix collapse to one **(quirk)**).
- **Shape:**

```json
{
  "preop":  { "anat": { "iso_T1w": "sub-01_ses-preop_acq-iso_T1w",
                        "ax_T2w":  "sub-01_ses-preop_acq-ax_T2w" } },
  "postop": { "anat": { "CT": "sub-01_ses-postop_CT" } }
}
```

  Post-op MRI keys are `ax_MRI`, `cor_MRI`, `sag_MRI`. `func`/`dwi` appear as sibling datatype objects (e.g. `"func": {"bold": …}`).
- Regenerated: after the classification window's OK, after the Replace/delete dialog, by the *Regenerate rawimage.json* button, after headless import, and automatically on dataset load for subjects with rawdata but no index. If the first pre-op value does not start with `sub-<id>_`, the index is considered stale ("Mismatched subjId") and regenerated.

#### 4.10 `dataset_description.json` and `.bidsignore`

| Created by | Content |
|---|---|
| Dataset selection, dataset load, legacy migration ("root folder" variant) | `Name` = root folder name, `BIDSVersion` = `1.6.0`, `LeadDBSVersion` = application version, `DatasetType` = `raw`; plus `.bidsignore` listing derivative patterns (`*CT*`, `*brainmask`, `*anchorNative*`, `*MNI152NLin2009bAsym*`, `*preproc*`, `*method.json`, `*.mat`, `*rawimages*`, `coregCT*`, `coregMR*`, `normalize*`, `export`, `miscellaneous`, `stimulations`, `atlases`, …) |
| Headless import (plain variant) | same four keys, no `.bidsignore` |

### 5. Output location summary

| Item | Path |
|---|---|
| Dataset root | user-chosen; folder name alphanumeric only |
| Staged DICOM | `sourcedata/sub-<id>/DICOM/` (or `…/DICOM/<uuid>/` for *Add DICOMs*); kept after conversion |
| Staged NIfTI / converter output | `rawdata/sub-<id>/unsorted/` (deleted after `okay`/`discard`/`empty`) |
| Classified images | `rawdata/sub-<id>/ses-{preop,postop}/{anat,func,dwi}/` |
| Subject index | `derivatives/leaddbs/sub-<id>/prefs/sub-<id>_desc-rawimages.json` |
| Per-subject UI prefs (electrode model, sides, post-op modality choice) | `derivatives/leaddbs/sub-<id>/prefs/sub-<id>_desc-uiprefs.mat` |

Subject id is fixed at staging time (§2.2). Multi-subject import = add several folders, then one *Run* with all highlighted subjects selected; windows appear one after another.

### 6. Which images the pipeline uses

#### 6.1 Pre-op anchor

1. Order list = `prefs.prenii_order` (default `T1w`, `T2w`, `PDw`, `T2starw`) followed by the template space's normalization-mapping modalities not already listed (space definition; exact list per template **(uncertain)**).
2. For each name in the order list, take all index keys that **contain** the name (substring, e.g. `iso_T1w`, `ax_T1w`); sort those by their acq prefix in the order `iso`, `ax`, `cor`, `sag`; append them.
3. Remaining keys appended in alphabetical order.
4. **Anchor = the first key** (e.g. `iso_T1w` beats `ax_T1w`; a subject with only `T2w` gets a `T2w` anchor). No pre-op image → anchor name falls back to the first `prenii_order` entry (and later steps fail).

All pre-op images in the index are used: each non-anchor pre-op image is coregistered to the anchor. There is **no per-image exclusion flag**; exclusion = not including it at import, or deleting it in the Replace/delete dialog.

#### 6.2 Post-op modality (CT vs MRI)

- Preference `prefs.preferMRCT`: `1` = MR, `2` = CT (default `2`).
- Per subject, the stored choice `uiprefs.modality` (`1` MR, `2` CT, `3` None) wins over the preference when the subject is loaded alone.
- GUI: drop-down *MR / CT / None* ("Modality of post-operative image (MR/CT/None) will be automatically detected. In case both MR and CT images are present, CT will be chosen by default."). It is **enabled only when both CT and MR exist** for a single selected subject; otherwise it just displays the detected modality. Changing it switches the reconstruction method list (MR: TRAC/CORE, Manual, Slicer; CT: Refined TRAC/CORE, TRAC/CORE, PaCER, Manual, Slicer, LeGUI) and is saved to `uiprefs`.
- Resolution: no `postop` in the index → `None`. Else MR present ⇔ the index has the key `ax_MRI` exactly; CT present ⇔ key `CT`. Choose MR if (prefer MR) or (prefer CT and no CT); choose CT if (prefer CT) or (prefer MR and no MR); neither → error "Post-op images not properly defined!". A cor/sag-only post-op MR therefore counts as **no MR** **(quirk)**; Phase C counts any orientation (`specs/01` §10 step 5).
- Post-op MR bundle: the first key containing `ax`, then `cor`, then `sag` (each optional).
- Several patients selected: the drop-down is disabled and forced to CT preference ("CT/MR modality will be automatically detected").

#### 6.3 Reconstruction method defaults

`prefs.reco.method.MRI` = TRAC/CORE (Horn 2015); `prefs.reco.method.CT` = PaCER (Husch 2017). In multi-patient runs these prefs override the GUI choice (§8.2).

### 7. Acceptance checks — Part 1

1. Adding a folder named `Pat_01-a` creates subject `Pat01a` with a warning; nothing asks for an id.
2. A DICOM folder is copied to `sourcedata/sub-<id>/DICOM/`; *Run* with *Import DICOM* converts into `unsorted/` with names `<folder>_<protocol>_<sequence>_<datetime>_<series>_<description>.nii.gz` + JSON, skipping localizers / derived images, then opens the classification window, then stops (no stage runs).
3. A file whose stem contains `mprage` pre-fills `preop / anat / T1w`, Include unticked; a stem containing `t2star` pre-fills `T2starw`; a stem containing `CT` anywhere pre-fills `postop / anat / CT` regardless of earlier matches.
4. Voxels `(1, 1, 1.02)` → `iso`; `(0.5, 0.5, 2)` → `ax`; `(2, 0.5, 0.5)` → `sag`; `(0.4, 3, 0.5)` → `cor`.
5. Two included rows producing the same name block *OK*; changing one row's Acquisition from `iso` to `ax` unblocks it.
6. *OK* with only post-op rows → "No preop files included"; with a post-op `MRI` row only `cor` → blocked "No postop MRI with the acquisition <ax>"; with no post-op rows → confirmation, then success.
7. After *OK*: images copied to `rawdata/sub-<id>/ses-*/anat/` with `.json`; `prefs/sub-<id>_desc-rawimages.json` lists keys `<acq>_<suffix>` / `CT` / `ax_MRI` with stems; `unsorted/` removed.
8. *Cancel → keep* leaves `unsorted/` and the subject highlighted; *Cancel → discard* removes it.
9. Anchor: index with `ax_T1w`, `iso_T1w`, `iso_T2w` → anchor `iso_T1w`.
10. Index with `CT` and `ax_MRI`, preference CT → post-op CT; uiprefs modality MR → post-op MRI; only `cor_MRI` → post-op `None`-like failure in the reference (Phase C: MRI).
11. Headless `lead import` with `…/T1_brain.nii` and `…/postopCT.nii` produces `sub-<id>_ses-preop_acq-<tag>_T1w.nii.gz`, `sub-<id>_ses-postop_CT.nii.gz` and the index.

---

## PART 2 — Batch processing

### 8. Selecting patients and sharing options

#### 8.1 Selection

- All subjects of the loaded dataset are listed; the user multi-selects rows. Selection order = list order (alphabetical by id). *Add patient* with several folders selects all newly added subjects.
- *Recent patients* / *Recent datasets* drop-downs re-open earlier selections.
- Command line: `lead dbs -<checkbox> [value] … <patient folder> <patient folder> …` (folders = `derivatives/leaddbs/sub-<id>`, all in one dataset; `-process` ticks the default pipeline: coregistration (CT: ANTs; MR: SPM), normalization (ANTs), brain-shift correction (coarse mask), reconstruction (Refined TRAC/CORE), electrode model index 1, no overwrite of approved results). Text values for method drop-downs are fuzzy-matched ("ANTs", "SynthMorph", "PaCER", …). The method pop-up window is suppressed during a command-line run.
- Job file: *Export Code* writes either a MATLAB job script that rebuilds the options and calls the run function, or a JSON dump of the options; `lead execute <file.json>` runs it without the GUI.

#### 8.2 Option sharing

One options record is built from the GUI once and shared by all selected subjects. Per-subject overrides when more than one subject is selected:

| Option | Source in multi-subject mode |
|---|---|
| Electrode model, sides | each subject's `uiprefs` (if present), else the GUI value |
| Post-op modality preference | forced CT preference; actual modality detected per subject (§6.2) |
| Reconstruction method | `prefs.reco.method.<CT|MRI>` per subject's post-op modality (GUI choice ignored) |
| Everything else (coreg / norm methods, checkboxes, overwrite-approved) | shared |

### 9. Processing order

- **Per patient, sequential:** patient 1 runs all ticked stages to the end, then patient 2, … (no per-stage batching).
- Within one patient, the fixed order is: import (exclusive, §2.4) → copy post-op raw to preprocessing → pre-op preprocessing (reorient, crop, bias correction; reslice anchor when post-op exists) → anchor pre-coregistration to template → AC/PC detection (optional) → pre-op MR coregistration → post-op MR or CT coregistration → checkreg figures for coregistration (if any coregistration ran) → normalization (skipped when approved; checkreg figures after) → brain-shift correction (skipped when approved) → **Check Results** (§11) → WarpDrive refine (optional) → reconstruction (sides already manually corrected are skipped unless overwrite) → refine localization window → surface extraction → connectome sub-routine → 2-D / 3-D output.
- **Optional parallelism:** if more than one patient is selected, a parallel toolbox is available and `prefs.pp.do = 1`, a worker pool of `prefs.pp.csize` (default 4) workers with profile `prefs.pp.profile` runs one patient per worker; default off.
- **Cluster:** the cluster run submits one job per patient through a site-specific submit function (examples shipped for HMS Orchestra and Martinos Launchpad); a job runs the same per-patient function and exits MATLAB. Helper filters select patients that had (or lacked) scheduler `.err` output or a given file, for re-submission.
- `prefs.env` (`dev`, `logtime`, `campus`) does not affect batching **(as far as observed)**.

### 10. Errors in a batch

- **More than one patient:** each patient's run is wrapped; an error prints a warning "<patient> failed. Please run this patient again and adjust parameters. Moving on to next patient." and the loop continues. No error file, no summary, no exit code (the error text itself is not printed with the warning). Same in the parallel pool.
- **One patient:** the error propagates to the user (dialog / console), run stops.
- After the loop the GUI reloads the patient list (not on command-line or exported runs).

### 11. Registration check in batch runs

- *Check Results* (Registration tab; tooltip "Manually checking results for each patient is highly recommended") is a normal stage in the per-patient order (§9), after brain-shift and **before** reconstruction.
- It first generates missing checkreg PNGs (coregistration and normalization), then opens the review window listing only images that are **not yet approved** (all pre-op coregistrations, the post-op coregistration — tone-mapped CT or all post-op MR —, the normalized anchor, the brain-shift result). If none is pending, nothing opens.
- **The review window does not block the run.** The run continues immediately with reconstruction of the same patient and then the next patient; in a batch, one review window per patient accumulates on screen. Approval / disapproval later writes the approval into the stage's method log (`coreg` per modality, `norm` and `brainshift` as one flag), which the next run honours: approved registrations are skipped unless *Overwrite approved* is set; approving normalization before all pre-op coregistrations triggers a warning that multispectral normalization will be redone.
- There is no "defer to the end" mode. The practical batch pattern is: run registration for all patients with *Check Results* off, then run again with only *Check Results* ticked (approved items are skipped, so only the review windows open), then run reconstruction.
- *Refine localization* likewise opens one non-blocking correction window per patient.

### 12. Hand-off to Lead Group and logs

- **Lead Group** has its own patient list: the user picks `derivatives/leaddbs/sub-<id>` folders (a group directory must be chosen first); each subject's electrode model is read from its `uiprefs`. There is no automatic hand-off from a batch run.
- **Logs written per patient:**

| Log | Path | When |
|---|---|---|
| Methods log (text, appended, timestamped paragraphs with references) | `derivatives/leaddbs/sub-<id>/log/sub-<id>_desc-methods.txt` | every stage that reports methods |
| Stage method logs (JSON incl. approval) | `coregistration/log/…`, `normalization/log/…`, `brainshift/log/…` (`…_desc-coregmethod.json` etc.) | each registration stage |
| Console transcript per stage | `<stage>/log/sub-<id>_desc-coreg{CT|MR}<yyyymmddTHHMMss>.log`, `…_desc-norm<timestamp>.log` | only when `prefs.diary = 1` (default 0) |
| Legacy migration report | `derivatives/leaddbs/logs/legacy2bids_naming.xlsx` | legacy import |

  No batch-level log or summary is written.

### 13. Acceptance checks — Part 2

1. Three patients selected, patient 2 fails in normalization: patients 1 and 3 complete; a warning names patient 2; no exception reaches the user.
2. One patient selected, same failure: the run stops with the error.
3. Stage order per patient is followed exactly (§9); patient 2 starts only after patient 1's last stage.
4. Multi-patient: each patient uses its own stored electrode model and sides; reconstruction uses the per-modality default method.
5. *Check Results* on with 3 patients: three review windows open during the run, none blocks; reconstruction runs before any approval.
6. Re-run with only *Check Results*: approved registrations are not recomputed; only pending images are listed.
7. `-process` on the command line runs coreg + norm + brain shift + reconstruction for every given folder without windows other than unavoidable review windows.
8. With `prefs.pp.do = 1` and ≥ 2 patients, patients run in parallel workers; failures warn and continue.

---

## 14. Lead-DBS behaviour vs current `specs/01` §8.4

| Lead-DBS behaviour | In `specs/01` §8.4 | Note for the implementer (state before 2026-10-22; the status column and the paragraph after the table give the current state) |
|---|---|---|
| Dataset root chosen in a dialog; `rawdata/`, `sourcedata/`, `derivatives/leaddbs/` created | yes | §8.4 Destination step |
| `dataset_description.json` when missing | partial | dbsloc writes `BIDSVersion 1.9.0`, no `LeadDBSVersion`, no `.bidsignore` |
| Subject id derived from folder name, cleaned silently | partial | dbsloc asks for the id with validation (better); offer folder name as default |
| Multi-subject import in one go (several folders → several subjects) | missing | wizard is one subject per run |
| DICOM kept in `sourcedata/sub-<id>/DICOM/` | missing | dbsloc converts into a temp dir; original DICOM not archived |
| Converted series staged in `rawdata/sub-<id>/unsorted/` and resumable later | missing | dbsloc keeps state only in the wizard |
| dcm2niix: gzip + BIDS sidecar | yes | |
| dcm2niix: ignore localizer / derived / 2-D (`-i y`) | missing | add `-i y` |
| File-name pattern with protocol, sequence, date-time, series number, description | partial | dbsloc uses `%s_%p_%d`; date-time not shown |
| Classification table with Include / session / type / modality / acq / run / task | partial | dbsloc: session, suffix, acq, remove; no run/task, no func/dwi |
| Lookup-table suggestion (regex on name, last match wins, user-editable, persisted) | partial | dbsloc: fixed rules on description + `Modality` tag (CT from DICOM `Modality`, more robust); not user-editable |
| Unmatched rows default to not included | missing | dbsloc defaults unmatched to pre-op T1w included — consider "unassigned" |
| Acq guess from voxel sizes | yes (different rule) | dbsloc: 10 % tolerance + affine to world axis (better than index-axis rule) |
| Image preview (3 planes, histogram, sidecar keys) per row | partial (2026-11) | the import wizard opens a 3-plane preview per row (`import_wizard.rs` `open_preview`, `dbsloc_viz::VolumePreview`); no histogram; sidecar keys not checked |
| Acquisition date surfaced / used for pre- vs post-op | missing in both | reference does not use it either; a date column + "later study = post-op" suggestion would be a Phase C improvement |
| Duplicate detection on resulting name, user resolves | yes | dbsloc `E_IMPORT_RULES` |
| `dir-` entity from phase encoding for bold/dwi | missing | out of scope (anat only) |
| At least one pre-op image | yes | `E_NO_ANCHOR` (also allows adding post-op to an existing subject) |
| No post-op → confirm and continue | partial | dbsloc allows silently |
| Post-op MR needs `ax` | deliberately differs | dbsloc: any of ax/cor/sag |
| Files copied to `ses-*/anat/` with BIDS names + JSON | yes | entity order: dbsloc follows BIDS (acq before run) |
| Index regenerated after import | yes (different file) | reference: `derivatives/leaddbs/sub-<id>/prefs/sub-<id>_desc-rawimages.json` (keyed map); dbsloc: `rawdata/sub-<id>/rawimages.json` (row list) and reads both (§10 step 2) |
| Re-classifying existing images when adding new ones (pre-ticked, backup/restore) | partial | dbsloc adds / replaces with conflicts + journaled rollback; cannot re-label an existing image in place |
| Replace / delete existing raw images | partial | dbsloc replace via conflicts; no delete; reference does not invalidate downstream, dbsloc does |
| Cancel keeps or discards staged files | partial | dbsloc rolls back everything (no staging to keep) |
| Import exclusive of processing in the same run | n/a | dbsloc import is a separate wizard |
| Legacy folder migration | missing | `specs/01` §8.3 non-goal; legacy name table in §2.8 above if ever needed |
| Headless import command | missing | candidate: `dbsloc import --root --subject --item session:suffix[:acq]=path` over `dbsloc_pipeline::import` |
| Per-subject post-op modality choice when CT and MR both exist | missing | reference: MR/CT/None choice stored per subject; dbsloc always picks MRI when both exist (`specs/01` §10 step 5) |
| Exclude an image without deleting it | missing in both | |
| Anchor = first by prenii order, acq order iso > ax > cor > sag | partial | dbsloc picks first suffix but per-suffix file by path sort, not acq order |
| Batch: per-patient sequential, continue on error with warning | missing (CLI / GUI single subject) | add a batch runner: per-subject loop, error captured per subject, summary table + exit code |
| Batch: per-subject electrode model / sides from subject prefs | partial | dbsloc already reads `uiprefs.mat` per subject |
| Checkreg during batch | differs | reference non-blocking windows per patient; dbsloc review hook pauses the run (GUI) — for batch, offer "review after all subjects" or run-with-review-off then review queue |
| Batch / per-patient logs | partial | reference has none at batch level; dbsloc session log exists; add per-run batch summary |

**Status 2026-10-22.** Closed by the import and batch work (`specs/01` §8.4 amendments, `00` §5.2, `03` §12.4a, `docs/RUN_REFERENCE.md`): multi-subject import (one subject per input folder), DICOM archived under `sourcedata/`, `-i y` and the full file-name pattern, lookup-table suggestions (last match wins, `DBSLOC_IMPORT_LOOKUP_JSON`; user-editable through the JSON, not yet in the GUI), unassigned rows, acquisition date column + "post-op = latest study" rule (opt-in), "no post-op → confirm", headless `dbsloc import`, per-subject post-op modality choice (CT preferred when both exist, `uiprefs.mat` `modality` honoured, `PostopModality`), batch runner with per-subject errors, summary table and exit codes, review *After all subjects* / deferred, batch summary. Image preview per row exists (volume preview in the import wizard). Still open: `run` / `task` entities, `.bidsignore`, resumable `unsorted/` staging, re-labelling an existing image in place, excluding an image without deleting it, acq order iso > ax > cor > sag for the anchor, regular-expression keywords.

