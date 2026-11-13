# 14 — Group analysis (Lead Group compatible)

> Specification written 2026-10-23 from the reference implementation (Lead-DBS 3.2.1 per `.version.txt`, git `5b1008d`, 2026-03-02) and the cited papers. No reference code is reproduced: behaviour is given as prose, mathematics, tables and file-format facts. Lead-DBS file and function names appear only as provenance pointers. Implementers read this document plus `specs/00`, `01`, `05`, `07`, `08`, `09`, `11`, `12`, `13`, `15`. Referenced as `specs/14` §n.

**Status:** implemented, partial (`docs/STATUS.md` row 12 lists the open items); specified 2026-10-23. Facts that could not be confirmed in the reference source or the papers are marked **unverified**. Statements that depend on `specs/07_vat.md` (stimulation settings, VTA / E-field stage) or `specs/15_connectomes.md` (connectome formats, fibre filtering inputs, seed maps), which were written in parallel, were marked **cross-reference to be aligned**; every such marker was resolved on 2026-10-23 against `specs/07` and `specs/15` (amendment notes at the changed places).

**Conventions.** Side index **1 = right hemisphere, 2 = left** (Lead-DBS order, same as Reco v2 keys, `specs/05` §8); `R` / `L` in file names. MATLAB classes are named as MATLAB names them (`double`, `char`, `cell`, `struct`, `logical`). Shapes are rows × columns. N = number of patients in the group, K = number of contacts of a lead. "Template space" = the normalization template space of the subjects (`specs/04` §3.1); Lead Group itself always works in the Lead-DBS installation's template space (default `MNI152NLin2009bAsym`). E-field magnitudes are in **V/m** in files and statistics, thresholds entered in V/mm are multiplied by 1000.

**Product decisions locked by this spec (owner, 2026-10-23).**

| # | Decision |
|---|----------|
| PD1 | Both use cases: research group analyses **and** single-patient programming support (a group of one, or a new patient scored against a stored group model, §7.6, §8.5, §9.4). |
| PD2 | First group release covers four statistics: atlas overlap tables (§6), sweet-spot mapping (§7), fibre filtering (§8), network mapping (§9). |
| PD3 | Full Lead Group migration: existing Lead Group analysis files and per-subject `ea_stats` files are **read** so cohorts open directly; files Lead Group can read back are **written** where feasible (§2.6, §3.4). |
| PD4 | Clinical outcome data are stored the way Lead Group stores them (inside the group file, `M.clinical`) plus a TSV export / import (§10). |
| PD5 | Every group feature runs on the desktop app, the CLI and the GPU server from the start (§4.6). |
| PD6 | VTAs come from the `vat` stage (`specs/07`), computed in native space, warped to template space, both stored. Group analyses read the **template-space** files. |

---

## 1. Purpose and scope

### 1.1 In scope (first group release)

| # | Feature | Lead-DBS provenance | Section |
|---|---------|---------------------|---------|
| G1 | Patient list from one or more BIDS datasets; add, remove, reorder; groups (cohort numbers) and group colours | `lead_group.m` (add / remove / move callbacks, drag and drop), `helpers/gui/ea_load_pts_group.m`, `helpers/gui/ea_refresh_lg.m` | §2, §4 |
| G2 | Stimulation settings per patient under the group label `gs_<guid>` | `setstimparamsbutton_Callback`, `helpers/group/ea_initializeS.m`, `helpers/ea_checkStimParams.m` | §4.2 |
| G3 | Batch VTA / E-field calculation over the cohort (the `vat` stage per subject) plus per-subject statistics | `calculatebutton_Callback`, `classes/leadgroup/ea_calc_biophysical_lg.m`, `ea_calc_vatstats.m` | §4 |
| G4 | Group 3D view: all leads in template space coloured by group or by a clinical variable, active / passive contacts, VTA surfaces, left → right mirroring, contact point clouds | `vizbutton_Callback`, `ea_elvis.m` group mode, `ea_showelectrode.m`, `ea_mirrorsides.m`, `ea_showisovolume.m` | §5 |
| G5 | Atlas overlap and contact-to-structure distance tables, group summaries, simple correlation / two-group tests | `ea_calc_vatstats.m`, `helpers/ea_vta_overlap.m`, `helpers/ea_efield_overlap.m`, `ea_showatlas.m` (`conmat`), `ea_lg_stats.m` | §3, §6 |
| G6 | Sweet-spot explorer (voxel-wise statistics over VTAs / E-fields vs outcome, cross-validation, prediction) | `explorers/sweetspot_explorer/*`, `explorers/stattests/*` | §7 |
| G7 | Fibre filtering explorer (group-level contract) | `explorers/fiberfiltering_explorer/*` | §8 |
| G8 | Network mapping explorer (group-level contract) | `explorers/networkmapping_explorer/*` | §9 |
| G9 | Clinical variables: entry, table import, storage in `M.clinical`, export | `addvarbutton_Callback`, `ea_edit_regressor.m`, `ea_refresh_lg.m` (clinical scores import) | §10 |
| G10 | Exports: TSV / CSV tables, NIfTI maps, scene screenshots, Lead Group `.mat` | `ea_lg_exportstats.m`, explorer exports | §11 |

### 1.2 Out of scope

- Lead-OR, Lead-Connectome tractography and the Lead Mapper connectivity computation itself (seed maps are an input from `specs/15`).
- The unified mapping explorer and Navigator (`explorers/unifiedmapping_explorer`, `explorers/navigator`), Lead Predict, microelectrode recording display (`M.ui.mer`), "miniset" generation (`ea_generate_min_dataset`), `lead_group_connectome` (`GroupConnectomeAnalysis.mat`).
- 2D group slices (`viz2dbutton`, `ea_writeplanes`) — later release.
- Reading or writing the explorers' saved MATLAB objects (`.sweetspot`, `.fibfilt`, `.netmap`, all v7.3 files holding a MATLAB class instance). They hold opaque class data that no MAT reader outside MATLAB decodes. We keep our own analysis records (§7.7, §8.6, §9.5).
- Pathway-activation (PAM / OSS-DBS axon) modes of fibre filtering.

---

## 2. Data model

### 2.1 Where Lead Group keeps its file

| Item | Fact |
|------|------|
| Group folder | `<dataset>/derivatives/leadgroup/<guid>/` (`helpers/ea_genGroupAnalysisFile.m`). The user picks either a dataset root (a folder with `derivatives/`; the group folder is then created under it) or an existing `derivatives/leadgroup/<id>` folder; the dataset name is the path component before `derivatives`. |
| File name | `dataset-<dataset>_analysis-<guid>.mat`, where `<dataset>` is the dataset folder name and `<guid>` is the analysis id. The finder (`helpers/ea_getGroupAnalysisFile.m`) accepts exactly the pattern `^dataset-<dataset>_analysis-<id>\.mat$` with `<id>` made of letters and digits only (no `_`); if several match it asks the user to choose. When the dataset folder name contains characters other than letters and digits, Lead Group cannot find its file on close and reports an error instead of saving. |
| `<guid>` | Default: the local creation time `yyyymmddHHMMSS` (`helpers/gui/ea_initializeM.m`); user-editable at creation; letters and digits only. |
| Variable | One variable, `M`. |
| MAT version | Lead Group creates and normally saves with `-v7.3` (HDF5). Several helpers (`ea_load_group.m`, `ea_checkStimParams.m`, `ea_lg_adaptptsfolder.m`, `ea_lg_renewguid.m`, `ea_migrateGroupAnalysis.m`, the new-programmer path in `lead_group.m`) save with the MATLAB default format (v7 unless the user changed the preference). Both kinds exist in practice. Lead-DBS always reads with a plain load, which accepts v6, v7 and v7.3. |
| Legacy (pre-BIDS) | `LEAD_groupanalysis.mat` in an arbitrary folder (**unverified** name; `helpers/ea_migrateGroupAnalysis.m` renames patients and writes the BIDS name). |
| Sibling files written by Lead-DBS | `…_desc-stats.mat` (atlas statistics of the group visualisation, `ea_showatlas.m`), `…_desc-stats_backup.mat`, `…_desc-stats_export.mat` (§11), `sweetspots/<id>.sweetspot`, `fiberfiltering/<id>.fibfilt`, `networkmapping/<id>.netmap`, `statvat_results/` (§5.6). |

### 2.2 The `M` structure

Field inventory from `ea_initializeM.m` (creation defaults), `ea_refresh_lg.m` (fields filled on every refresh), `lead_group.m` callbacks and the explorers. "Persisted" = present in a saved file in normal use. Every field may be missing in an old file (§2.5).

#### 2.2.1 Top level

| Field | Class / shape | Content | Persisted |
|-------|---------------|---------|-----------|
| `patient.list` | `cell` N×1 of `char` | Absolute path of each patient's derivative folder `…/derivatives/leaddbs/sub-<id>` (no trailing separator). The folder name is the patient name. | always |
| `patient.group` | `double` N×1 | Group (cohort) number per patient, positive integers, default 1 for added patients; the GUI changes it by ±1 and never below 1. | always |
| `groups.group` | `double` G×1 | Sorted unique values of `patient.group`. Rebuilt whenever it differs from them. | after the first refresh with patients |
| `groups.color` | `double` G×3, RGB in [0, 1] | Group colours (§2.2.3). | as above |
| `groups.colorschosen` | `double` 1 | Present (value 1) when colours were set; only drives a button colour. | optional |
| `clinical.vars` | `cell` 1×C of `double` matrices N×w | One matrix per clinical variable (§10.1). | always (may be empty) |
| `clinical.labels` | `cell` 1×C of `char` | Variable names, same order as `vars`. | always |
| `S` | `struct` 1×N, or `[]` | Stimulation settings per patient (the `S` structure of `specs/07` §2.2; `label` forced to `gs_<guid>`; aligned 2026-10-23 with `07` §2.2). | after stimulation parameters are set |
| `vatmodel` | `char` or `[]` | Display name of the VTA model used by the group (`'Kuncel 2008'`, `'Maedler 2012'`, `'Dembek 2017'`, `'Fastfield (Baniasadi 2020)'`, `'SimBio/FieldTrip (see Horn 2017)'`, `'OSS-DBS (Butenko 2020)'`). Lead-DBS keeps it equal to `S(1).model` and uses one model for the whole group. | after stimulation |
| `elstruct` | `struct` 1×N | Per-patient electrode data in **template space** (§2.2.2). Rebuilt from each patient's reconstruction on every refresh; deleted when patients are reordered. | after the first refresh |
| `stats` | `struct` 1×N with one field `ea_stats` | Copy of each patient's `sub-<id>_desc-stats.mat` (§3), reduced to the stimulation entries whose label is `gs_<guid>`. Deleted when patients are reordered or the guid is renewed. | after Calculate |
| `vilist` | `cell` of `char` | Atlas structure names of the statistics (`ea_stats.atlases.names`, file names with `.nii` / `.nii.gz`). | after Calculate |
| `fclist` | `cell` of `char` | Parcellation labels of the fibre-count statistics (`ea_stats.stimulation(1).ft(1).labels{1}`). | when connectivity stats ran |
| `isomatrix` | `cell` 1×r | The clinical variable(s) currently selected in the clinical list (a copy of `clinical.vars(selection)`). | refresh |
| `isomatrix_name` | `cell` 1×r of `char` | Their labels. | refresh |
| `guid` | `char` | Analysis id (§2.1). Legacy files without it get one at load. | always |
| `root` | `char` | Group folder with a trailing path separator. | always |
| `ui` | `struct` | View state (§2.2.4). | always |
| `pseudoM`, `ROI.list`, `ROI.group` | — | Explorer input made from a list of NIfTI files instead of patients. Not produced by Lead Group; read-only support not required. | rare |
| `merstruct` | — | Microelectrode recordings (developer mode). Ignored. | rare |
| `stimparams` | — | Deprecated; removed by Lead Group on refresh. Ignored. | legacy |

#### 2.2.2 `elstruct(p)`

| Field | Class / shape | Content |
|-------|---------------|---------|
| `name` | `char` | Patient folder name `sub-<id>`. |
| `elmodel` | `char` | Electrode model (`specs/09` name). From the patient's reconstruction (`props.elmodel`) unless the group forces a model (`ui.elmodelselect` > 1); `'Medtronic 3389'` when an old reconstruction stores none. |
| `coords_mm` | `cell` 1×2 of K×3 `double` | Template-space contact centres, **recomputed from the markers** with the model geometry (the same marker → contacts affine as `specs/05` §6 / `specs/12` §4). Empty `[]` for an absent side. |
| `trajectory` | `cell` 1×2 of M×3 | Template-space trajectory (50 rows, `specs/12` §3.2). |
| `markers` | `struct` 1×2 with `head`, `tail`, `x`, `y` (1×3 each, `[]` for an absent side) | Template-space markers. |
| `coords_acpc` | `cell` 1×2 or NaN | AC/PC coordinates if the reconstruction has them. |
| `group` | `double` | Copy of `patient.group(p)`. |
| `groupcolors` | `double` G×3 | Copy of `groups.color`. |
| `groups` | `double` G×1 | Copy of `groups.group`. |
| `activecontacts` | `cell` 1×2 of K×1 logical | Set from `S(p).activecontacts` only while the 3D view is built; not normally persisted (the file is saved before the copy). |

Every patient gets both sides: a missing side is filled with empty `coords_mm`, `trajectory` and empty markers so all patients have the same number of sides (`ea_refresh_lg.m`).

#### 2.2.3 Default group colours

When groups change, colour of group number g = entry g of a fixed list of 36 colours with the HSV saturation halved. The list is the concatenation of eight published palettes in this order (hex): `#1DACE8 #1C366B #F24D29 #E5C4A1 #C4CFD0` · `#D8A49B #C7CEF6 #7496D2` · `#B62A3D #EDCB64 #B5966D #DAECED #CECD7B` · `#9A872D #F5CDB6 #F7B0AA #FDDDA4 #76A08A` · `#F8DF4F #A35E60 #541F12 #CC8B3C #E8D2B9` · `#AEA8A8 #CB9E23 #957A6D #AC6E49` · `#456355 #FCD16B #D3DDDC #C6B19D` · `#DBB165 #DEB18B #2E604A #27223C #D1362F` (`ea_color_wes('all')`). A file without `groups` is shown with grey `[0.7 0.7 0.7]`.

**Reference defect (do not reproduce):** the rebuild indexes the colour list by group *number* while the 3D view indexes `groupcolors` by group *number* and the colour picker by *position*. With non-contiguous numbers (for example groups {1, 3}) the view reads past the end of the table. Our writer therefore always renumbers groups to 1…G before writing `M` (§2.6), keeping the user's labels in our JSON.

#### 2.2.4 `M.ui`

Creation defaults from `ea_initializeM.m`; further fields are added by `lead_group.m` and `ea_refresh_lg.m`. All numeric fields are `double` scalars unless stated.

| Field | Default | Meaning |
|-------|---------|---------|
| `listselect` | 1 | Selected patient indices (row vector); the explorers use it as the default patient selection. |
| `clinicallist` | 1 | Selected clinical variable index (or indices). |
| `hlactivecontcheck` | 0 | Highlight active contacts. |
| `showpassivecontcheck` | 1 | Show inactive contacts. |
| `showactivecontcheck` | 1 | Show active contacts. |
| `showisovolumecheck` | 0 | Show the clinical-variable isovolume (§5.6). |
| `isovscloudpopup` | 1 | Isovolume display: 1 = point cloud, 2 = smoothed isosurface (labels **unverified**, stored in the `.fig`). |
| `volumeintersections` | 1 | Selected atlas structures in the statistics explorer. |
| `fibercounts` | 1 | Selected parcels in the statistics explorer. |
| `elrendering` | 1 | Electrode rendering style index (labels **unverified**). |
| `statvat` | 0 | Export the "statvat" VTA-regressor maps when the view is built (§5.6). |
| `elmodelselect` | 1 | 1 = each patient's own model, k > 1 = force catalogue entry k − 1 for all. |
| `normregpopup` | 1 | Regressor normalisation for colouring: 1 = none, 2 = z-score (NaN-aware), 3 = van Albada & Robinson 2007 rank-to-normal transform. |
| `colorpointcloudcheck` | 0 | Colour contacts by the selected clinical variable (§5.3). |
| `mirrorsides` | 0 | Mirror all leads (and VTAs) to the other hemisphere (§5.4). Missing in old files; Lead-DBS then uses 0. |
| `showdiscfibers` | 0 | Show fibre-filtering results in the view (no effect in Lead Group itself, **unverified**). |
| `lc.graphmetric`, `lc.normalization`, `lc.smooth` | 1, 1, 1 | Lead-Connectome options; carried, unused. |
| `atlassetpopup` | name of the default atlas | Atlas set. **Old files store an index (number)** into the folder listing of the installation's atlases; new files store the atlas name. |
| `labelpopup` | default parcellation | Parcellation for connectivity statistics. Index (old) or name (new). |
| `connectomename` | last entry of the connectome list (`'Do not calculate connectivity stats'`) | Structural connectome for connectivity statistics. |
| `lastupdated` | — | Time of the last refresh as the number `yyyyMMddHHmmss`. |
| `regressorcolormap` | — | Optional colour map (n×3) for regressor colouring. |
| `stimSetMode` | — | OSS-DBS stimulation-set mode flag. Carried, unused. |
| `mer` | — | Developer flag. Carried, unused. |

### 2.3 Our representation: the group record

The group record is a JSON file in the group folder, **primary** for our software. The `.mat` twin (§2.6) is derived from it and kept in step.

- **Group folder.** Default `<dataset>/derivatives/leadgroup/<guid>/` (same as Lead-DBS). It does **not** move with `RunOptions.output_dir`: the group folder is chosen when the group is created and stored as an absolute path by the front end; subjects keep their own derivative roots.
- **File:** `<group folder>/dataset-<dataset>_analysis-<guid>.json` (schema id `dbsloc-group/1`). The name mirrors the MAT twin; Lead-DBS's finder ignores it because it looks only for `.mat`.
- **Writes** are atomic (write `.partial-*` and rename, `specs/00` §5.2), 2-space indented, trailing newline.

```text
{
  "schema": "dbsloc-group/1",
  "guid": "20261023101500",
  "dataset_name": "MyCohort",
  "title": "free text",
  "created_at": "<RFC 3339 UTC>", "updated_at": "<RFC 3339 UTC>",
  "software": {product, version, git_hash},            // as Reco v2 meta.software (specs/05 §8)
  "datasets": [{"id": "d1", "root": "<abs path or server dataset id>", "derivatives_root": "<abs path>"}],
  "patients": [{
      "subject": "sub-01", "dataset": "d1",
      "group": 1,
      "elmodel_override": null,                          // or a specs/09 model name
      "included": true
  }],
  "groups": [{"number": 1, "label": "STN", "color": [r,g,b]}],
  "stimulation": {
      "label": "gs_20261023101500",
      "model": "<specs/07 §1.1 model id>",               // efield | kuncel | maedler | dembek (aligned 2026-10-23 with 07 §1.1)
      "settings_ref": "per_subject"                      // settings live in each subject's stimulation folder
  },
  "clinical": [{ ... §10.1 ... }],
  "atlas_set": "<specs/11 atlas id>",
  "view": { ... the M.ui fields we model, §2.4 ... },
  "analyses": [{"kind": "sweetspot|fiberfiltering|networkmapping|atlasstats", "id": "...", "record": "<relative path>"}],
  "lead_group": {
      "mat_file": "dataset-MyCohort_analysis-20261023101500.mat",
      "mat_sha256": "<hash of the twin as last written or imported>",
      "passthrough": { ... §2.6 ... },
      "dropped_fields": ["M.ui.mer", ...]
  }
}
```

Patient order in `patients` **is** the Lead Group patient order (row order of every `M` per-patient field and of every clinical variable).

### 2.4 Mapping table M ↔ group record

| `M` field | Group record | Direction / rule |
|-----------|--------------|------------------|
| `patient.list{p}` | `patients[p].subject` + `datasets[…]` | Read: the last path component is the subject, the folder above `sub-<id>` is the derivative root, the folder above `derivatives` is the dataset root. Write: absolute path `<derivatives_root>/sub-<id>` on the desktop; on the server see §4.6. |
| `patient.group(p)` | `patients[p].group` | Written renumbered to 1…G in ascending order of our group numbers. |
| `groups.group`, `groups.color` | `groups[]` | `groups.group` = 1…G (column), `groups.color(g,:)` = colour of group g. `colorschosen` = 1 when any colour was set explicitly. |
| `clinical.vars`, `clinical.labels` | `clinical[]` | §10.4. Lossless for numbers and NaN. |
| `S(p)` | per-subject canonical settings `stimulations/native/gs_<guid>/sub-<id>_desc-stimulation.json` (`specs/07` §2.3) and its derived MAT twin `sub-<id>_desc-stimparameters.mat` in every space folder of the label (`07` §5.1) | The canonical JSON is primary; `S(p)` is its `07` §2.4 export with `label = gs_<guid>`; reading an `M` imports each `S(p)` into the canonical JSON by the same mapping. We never write `sub-<id>_desc-stimparameters.json` (`07` §2.1, decision O2). Aligned 2026-10-23 with `07` §2.3, §2.4, §5.1. |
| `vatmodel` | `stimulation.model` | Display name ↔ model id table of `specs/07` §1.1. A group whose `vatmodel` is `Fastfield (Baniasadi 2020)` or `OSS-DBS (Butenko 2020)` opens and its existing outputs are read, but *Calculate* fails per subject with `E_VAT_model` (`07` §10) until a computable model is chosen (aligned 2026-10-23). |
| `elstruct(p)` | — (derived) | Rebuilt at write from the subject's Reco v2 template block (`mni`, or `reference` when the subject's template space is not Lead-DBS's, `specs/12` §7) with contacts recomputed from the markers. Not stored in JSON. |
| `stats(p).ea_stats` | — (derived) | Read from each subject's statistics (§3); written into the twin for convenience. |
| `vilist`, `fclist` | — (derived) | From the statistics. |
| `isomatrix`, `isomatrix_name` | `view.colour_by` | Written as the selected clinical variable(s). |
| `guid` | `guid` | Identity. |
| `root` | — | Written as the twin's folder with a trailing separator. |
| `ui.listselect`, `ui.clinicallist`, `ui.mirrorsides`, `ui.colorpointcloudcheck`, `ui.showactivecontcheck`, `ui.showpassivecontcheck`, `ui.hlactivecontcheck`, `ui.normregpopup`, `ui.elmodelselect`, `ui.showisovolumecheck`, `ui.isovscloudpopup` | `view.*` with the same meaning | Lossless. |
| `ui.atlassetpopup` | `atlas_set` | Read: a number is resolved against the alphabetical list of atlas folders of the Lead-DBS installation **if** the user configured one (`DBSLOC_LEADDBS_ATLASES_DIR`, `specs/11` §1), else kept as unresolved with a warning. Write: the atlas name (new format). |
| `ui.labelpopup`, `ui.connectomename` | `view.parcellation`, `view.connectome` | Names; numbers are resolved like the atlas index, else kept verbatim. |
| everything else (`ui.lc.*`, `ui.lastupdated`, `ui.statvat`, `ui.elrendering`, `ui.regressorcolormap`, `ui.stimSetMode`, `ui.volumeintersections`, `ui.fibercounts`, unknown fields) | `lead_group.passthrough` | Kept verbatim (§2.6). |

### 2.5 Read rules (missing and legacy fields)

Opening a Lead Group file (`.mat` v6/v7 via `fastwarp-matfile` `read_mat5`, v7.3 via `MatFile`; `specs/12` §2):

1. The file must contain a variable `M` that is a struct; otherwise `E_LG_NOT_GROUP`.
2. `patient.list` missing or not a cell of char → `E_LG_NOT_GROUP`. A row vector (1×N) is accepted as N×1.
3. `patient.group` missing or of the wrong length → all 1 with warning `W_LG_GROUPS`.
4. `guid` missing → take `<id>` from the file name; else the file's modification time `yyyymmddHHMMSS`.
5. `clinical` missing → no variables. A variable whose row count ≠ N → `W_LG_CLINICAL_ROWS`, the variable is kept in `passthrough` and not offered for analysis.
6. `S` present: each element is imported with the rules of `specs/07` §2.2 (files without `ver`: right contact n ← `k(n−1)`, left contact n ← `k(n+7)`; missing `sources`, `volume`, `numContacts` added; contact-count mismatch padded or trimmed; reference provenance `ea_checkStimParams.m`) and mapped to the canonical JSON by `07` §2.4. Aligned 2026-10-23 with `07` §2.2, §2.4.
7. Patient folders that do not exist: if the group folder sits at `<x>/derivatives/leadgroup/<guid>/` and `<x>/derivatives/leaddbs/<name>` exists for the **first** patient, all paths are remapped to `<x>/derivatives/leaddbs/<name>` (Lead-DBS checks only the first patient, `ea_checkremap_lg.m`; we check every patient and report each one that stays missing as `W_LG_PATIENT_MISSING`). A `miniset.json` beside the dataset forces the same remap (`ea_load_group.m`).
8. `ui.atlassetpopup` / `labelpopup` numeric → §2.4.
9. `stats` is never trusted as the source of statistics: it is compared with the subjects' own statistics files; a mismatch is reported (`W_LG_STATS_STALE`) and the subject files win.
10. MATLAB classes the readers do not decode (objects, function handles, `string`, `table`, sparse) become `Unsupported`; their fields are listed in `lead_group.dropped_fields` and never written back.
11. Unknown fields anywhere in `M` → `passthrough` (if decodable) or `dropped_fields`.

### 2.6 Writing the Lead Group twin

- **Format:** MAT **v7** (Level-5, compressed elements), one variable `M` (`fastwarp/crates/fastwarp-matfile` `write_mat5`). Lead-DBS reads it with its plain load; on its next save Lead Group converts it to v7.3 itself. We do not write v7.3 (no HDF5 writer; decision D1).
- **Name and place:** `<group folder>/dataset-<dataset>_analysis-<guid>.mat`. If `<dataset>` or `<guid>` contain characters other than letters and digits, the file is still written but `W_LG_NAME` says that Lead Group will not find it.
- **Before the first overwrite** of a file we did not write (its hash ≠ `mat_sha256`): copy it to `dataset-<dataset>_analysis-<guid>_desc-backup<UTC yyyymmddHHMMSS>.mat` (the underscore keeps it outside Lead-DBS's finder pattern) and import its changes first (§2.7).
- **Content:** every field of §2.2 that we can derive, in this order: `patient`, `guid`, `clinical`, `vilist`, `fclist`, `ui`, `S`, `vatmodel`, `root`, `groups`, `elstruct`, `isomatrix`, `isomatrix_name`, `stats`; then `passthrough` fields at their original paths. Shapes as §2.2 (cells N×1 for `patient.list`, 1×C for `clinical.*`, struct arrays 1×N). Numbers `double`, flags `double` 0/1.
- **Size guard:** a v7 variable larger than 2 GB cannot be written; then `stats` is omitted (Lead Group rebuilds it from the subject files on refresh) and `W_LG_STATS_OMITTED` is logged.
- The twin is rewritten whenever the group record changes and at the end of every group job.

### 2.7 Edits made in Lead Group

On every open, if the twin's SHA-256 differs from `lead_group.mat_sha256`, the twin was changed outside our software. Its patient list, groups, colours, clinical variables, `S` and `ui` are imported into the group record (the twin wins for these fields), the user sees `Imported changes made in Lead Group: <fields>`, and the hash is updated. Derived fields (`elstruct`, `stats`) are ignored.

---

## 3. Per-subject statistics file (`ea_stats`)

### 3.1 Location and format

- `<derivatives root>/sub-<id>/sub-<id>_desc-stats.mat` (subject folder root; `BIDSFetcher.getStats`), one variable `ea_stats`.
- Lead-DBS writes it with `-v7.3`. Atlas-statistics changes keep a copy `sub-<id>_desc-stats_backup.mat` when the atlas set changed.
- Written by two producers: the 3D scene's atlas pass (contact distances, `ea_showatlas.m`) and the VTA statistics (`ea_calc_vatstats.m`); the connectivity pass adds `ft` (`ea_cvshowfiberconnectivities.m`).

### 3.2 Structure

| Field | Class / shape | Content |
|-------|---------------|---------|
| `atlases.names` | `cell` 1×A of `char` | Atlas structure file names (with `.nii` / `.nii.gz`). |
| `atlases.types` | `double` 1×A | Atlas type per structure (`specs/11` §3: 1 right-only, 2 left-only, 3 both sides in two files, 4 mixed file, 5 midline). An old format holds the whole atlas table (field `rebuild` present); Lead Group then strips it to `names` + `types`. |
| `electrodes` | `struct` (elstruct, §2.2.2) | The electrode(s) the statistics were made for. |
| `patname` | `cell` E×2 of `char` | Patient name per electrode and side. |
| `conmat` | `cell` E×2 of K×A `double` | Distance in mm from each contact to the nearest supra-threshold voxel centre of each structure (nearest-neighbour search over the voxels of that structure and side above the atlas display threshold, `specs/11` §4.2). NaN for structures that were not computed. |
| `conmat_inside_hull` | as `conmat` | `conmat`, with 0 where the contact lies inside the structure's surface mesh. |
| `conmat_inside_vox` | as `conmat` | `conmat`, with 0 where the distance is below the mean voxel size of the structure. |
| `stimulation(k).label` | `char` | Stimulation label (folder name), e.g. `gs_<guid>`. One entry per label; entries are found by label and appended when new. A file whose `stimulation` has no `label` field is legacy: the whole `stimulation` is discarded. |
| `stimulation(k).vat(s, j)` | `struct` 2×J (J = number of VTA meshes per side, normally 1) | Fields: `amp` (1×4 `double`, the side's source amplitudes `S.amplitude{s}`), `label`, `contact` (= j), `side` (= s), `volume` (mm³, the binary VTA voxel count × voxel volume), `AtlasIntersection` (1×A, mm³), `nAtlasIntersection` (1×A, **fraction of the VTA** in [0, 1]), `nWithinAtlasIntersection` (1×A, **fraction of the structure** in [0, 1]), `AtlasVolume` (1×A, mm³; NaN when not computed). |
| `stimulation(k).efield(s, j)` | `struct` | Only when an E-field file exists: `volume` = Σ E over voxels with E above the threshold (V/m · voxels, **not** a volume despite the name), `AtlasIntersection` (1×A, Σ E inside the structure), `nAtlasIntersection` (1×A, fraction of Σ E inside the structure). |
| `stimulation(k).ft(s)` | `struct` | Connectivity statistics: `fibercounts{1}` (1×P, fibres from the VTA ending in each parcel ÷ total fibres of the connectome), `nfibercounts{1}` (`fibercounts` ÷ VTA volume), `labels{1}` (1×P parcel names). Read and carried through unchanged. `specs/15` does not define parcel fibre counts, so our writers do not compute `ft` in the first release (open item, noted 2026-10-23). |

### 3.3 How the values are computed (reference behaviour)

For each side s with any non-zero amplitude, each structure a:

1. **Atlas file per side** by type: 1 → `rh/<name>`, 2 → `lh/<name>`, 3 → `rh/` or `lh/` by side, 4 → `mixed/<name>`, 5 → `midline/<name>`. Non-NIfTI entries (fibre atlases) get 0 overlap and NaN volume.
2. **Hemisphere split:** voxels of the atlas whose world x is on the other side of 0 (x < 0 for the right side, x > 0 for the left; RAS) are set to 0.
3. **Binarise:** VTA voxel > 0.5 · max(VTA); atlas voxel > 0.5 · max(atlas).
4. **VTA-grid overlap:** the binarised atlas is resampled onto the VTA grid with trilinear interpolation and binarised again at 0.5. `vox_overlap` = number of VTA voxels inside both. `AtlasIntersection` = `vox_overlap` × VTA voxel volume; `nAtlasIntersection` = `vox_overlap` ÷ number of VTA voxels (0 when the VTA is empty).
5. **Atlas-grid overlap:** the binarised VTA is resampled onto the atlas grid (trilinear, then > 0.5). `nWithinAtlasIntersection` = overlap voxels on the atlas grid ÷ atlas voxels. `AtlasVolume` = atlas voxels × atlas voxel volume.
6. `volume` of the VTA is overwritten by the voxel-count volume of step 3.
7. **E-field:** voxels with E ≤ `horn_ethresh` × 1000 V/m are set to 0 (the threshold of the SimBio model settings, used whatever the model, **verified** in `ea_efield_overlap.m`); `efield.volume` = Σ E; the atlas (side-split, not pre-binarised) is resampled onto the E-field grid (trilinear) and binarised at 0.5 · max; `AtlasIntersection` = Σ E · mask; `nAtlasIntersection` = that ÷ Σ E (0 when Σ E = 0).
8. With all amplitudes of a side zero: every overlap 0, `AtlasVolume` NaN.

**Reference defect (not reproduced):** in step 4–5 the reference applies the original 0.5 · max thresholds a second time to the already binarised images, so an atlas whose maximum exceeds 2 (for example a uint8 mask with value 255) gives zero overlap. We binarise once and threshold the resampled image at 0.5.

**Our values (aligned 2026-10-23 with `07` §8).** The `vat` stage computes the `vat` and `efield` quantities by `specs/07` §8.1–§8.3, which differs from the reference steps above in two further places: `nWithinAtlasIntersection` is taken from the VTA-grid intersection (step 5 is not done on the atlas grid), and the E-field threshold of step 7 is the stimulation label's own threshold (`07` §2.5), not the SimBio preference.

### 3.4 Our writer rules

- **Primary record (aligned 2026-10-23 with `07` §5.1, §8.4):** `sub-<id>_desc-vtastats.json` in each template-space (and reference-space) label folder `sub-<id>/stimulations/<space>/<label>/`, written by the `vat` stage (`07` §8.4); its per-structure quantities carry the §3.2 field names, side keys `"1"` / `"2"`. This spec defines no second statistics JSON.
- **Lead-DBS twin:** `sub-<id>/sub-<id>_desc-stats.mat`, MAT v7 (`07` §5.2). The `vat` stage updates only the `stimulation` entry of its label, under the conditions of `07` §8.4 (file absent: created; fully readable with equal `atlases.names`: the entry is replaced or appended and every other field kept; otherwise left untouched with the `07` §8.4 note). The group contact-distance pass refreshes `atlases`, `electrodes`, `patname` and `conmat*` and keeps every other field as read. Neither writer overwrites a file that holds an `Unsupported` value (`W_STATS_MAT_SKIPPED`; the JSON records are still written).
- `conmat*` are computed by the group statistics job for every patient (no VTA needed, §6.2).
- **Atlas-set change** (only the group contact-distance pass changes `atlases`; the `vat` stage never does): the old file is copied to `sub-<id>_desc-stats_backup.mat` (reference behaviour), then written with the new `atlases` and `conmat*`; its `stimulation` entries, computed against the old set, are not carried over (their `sub-<id>_desc-vtastats.json` records stay). The pass therefore runs **before** `vat` in *Calculate* (§4.1), so the `vat` stage finds matching `atlases.names` and adds its entry. Aligned 2026-10-23 with `07` §8.4; dropping the old entries is a decision of this alignment, open for review.

---

## 4. Group pipeline semantics

### 4.1 Stages per subject

The group never runs stages of its own on subjects; it runs subject stages in a batch (`specs/00` §5.2, `specs/13` Part 2 semantics: sequential subjects, continue on error, summary table, exit code 1 on any failed subject, 130 on cancel):

| Group action | Subject stages run | Notes |
|--------------|--------------------|-------|
| *Calculate* (G3) | the contact-distance pass (§3.4) when the subject's `sub-<id>_desc-stats.mat` is missing, records another atlas set or has no `conmat*`; then `vat` with `RunOptions.vat_labels` = [`gs_<guid>`] and `vat_stats_atlas` = the group's `atlas_set` (`07` §7.2, §8), which writes the atlas statistics itself (`07` §8.4) | Prerequisite per subject: `reconstruct` done and the canonical settings `stimulations/native/gs_<guid>/sub-<id>_desc-stimulation.json` (`07` §2.3). Subjects without either are reported, not run. `gs_*` labels are computed only by a group job (`07` §7.2). Aligned 2026-10-23. |
| *Calculate* with mirroring on | additionally the flip of each VTA / E-field (§4.4) | |
| *Contact distances* | `stats` without VTA (`conmat*`) | Works on reconstruction only. |

The batch order is the group's patient order. Selection = all included patients, or the selected rows.

### 4.2 Stimulation label and settings

- Label `gs_<guid>` (Lead-DBS: every group stimulation is relabelled `gs_<guid>` on refresh). Folder `sub-<id>/stimulations/<space>/gs_<guid>/` for the template space and `stimulations/native/gs_<guid>/` for native (PD6). The canonical settings file is `stimulations/native/gs_<guid>/sub-<id>_desc-stimulation.json`; every space folder of the label gets the derived `sub-<id>_desc-stimparameters.mat` (`07` §2.3, §5.1; aligned 2026-10-23).
- Per-subject settings are edited in the stimulation editor of `specs/07` in group mode (one row per patient) and stored as each subject's canonical JSON. On open the group compares each subject's settings with the group record's `S` (the twin) and, as Lead-DBS does, the **group wins**: a differing subject file is overwritten, logged `Local stimulation settings of sub-<id> differed from the group and were replaced.`
- **Desktop group mode (amended 2026-10-25).** The group panel offers *Edit* per patient. It opens the stimulation editor of `specs/07` for that patient on the label the group computes from: the *Settings from* source label when one is chosen (created for the patient when missing; the next *Compute VTAs* copies it into `gs_<guid>`), otherwise `gs_<guid>` itself. In `gs_<guid>` mode the editor shows only that label (no *New…* / *Duplicate* / *Delete*), allows the `gs_` prefix for it alone, and offers no *Compute VTA* (`gs_*` labels are computed by the group job only, `07` §7.2). Saving writes the canonical JSON as for any label.
- **Settings source (amended 2026-10-25).** *Calculate* may name a source label of the same subject (`--copy-settings-from <label>`, GUI *Settings from*). Without `--sync-settings` it only seeds a missing `gs_<guid>` file from the source. With `--sync-settings` (the GUI always sets it when a source is chosen) the `gs_<guid>` file of a subject that has the source label is replaced by a copy of the source (label and subject rewritten) whenever the two differ in anything but the label, so the group follows later edits of the source; a subject without the source keeps its existing `gs_<guid>` settings, and one without either fails `not_run` as before. An unchanged copy is not rewritten, so the `vat` fingerprint and the outputs stay valid.
- One VTA model per group (Lead-DBS restriction; `M.vatmodel`).
- Copying a group under a new guid (`ea_lg_renewguid.m` behaviour) relabels every `S` and removes `stats`.

### 4.3 Space

- Group analyses read only **template-space** files `stimulations/<template space>/gs_<guid>/…` (PD6). The native files exist for single-patient use.
- All subjects of a group must share one template space for statistics. A subject in another space is used only if the group space is `MNI152NLin2009bAsym`: its leads through its `reference` block / bridge (`specs/04` §5.4a), its VTA / E-field files from the reference-space copy that the `vat` stage writes to `stimulations/MNI152NLin2009bAsym/<label>/` (`07` §4 step 3; aligned 2026-10-23). Without that copy (no bridge) it is excluded from voxel statistics with `W_GROUP_SPACE` and kept in the 3D view only.

### 4.4 Mirrored (flipped) volumes

- **Flip transform:** a non-linear left-right transform of the group template: mirror the template about x = 0 (negate x of the affine), register template → mirrored template with the in-process SyN engine (`specs/04` §5; all template contrasts, equal weights), store forward and inverse composites once per template space in the template cache (`<templates>/<space>/fliplr/from-<space>_to-<space>Flipped_mode-image_xfm.h5` and inverse). Lead-DBS offers a downloadable transform for `MNI152NLin2009bAsym` or builds it the same way with ANTs (`ea_genflipspace.m`); importing the Lead-DBS one (`<leaddbs>/templates/space/<space>/fliplr/Composite.nii.gz`) is not implemented yet; planned (owner decision 2026-11-13), convention to be specified.
- **Images:** flip the image (negate x of the affine), then apply the transform; linear interpolation for E-fields, nearest neighbour for binary VTAs. The flipped file is written on the **mirrored grid of the source image** (same dimensions, x of the affine negated, so a grid covering x = a … b mm becomes −b … −a mm), with the plain flip and with the transform alike: the transform is applied by resampling onto that mirrored grid, never onto the source grid, which holds the opposite hemisphere when the source grid is not symmetric about x = 0 (corrected 2026-11-12). Readers take each file's own affine. The readers check only that the header puts a flipped file on the mirrored grid of its original (`flip::flipped_is_current`); a flipped file on the right grid made from an older version of the original cannot be detected that way. The *File names* rule is implemented in *Calculate* (2026-11-12): whenever the `vat` step of a patient replaced or removed any file in a template-space folder of the group label (seen by `outputs::ReplaceWatch` on the staged commit: it computed the label with template-space outputs, or its commit failed part-way after replacing a template-space file; writes to the `.partial-` temporary files before the commit do not count), every flipped file of that label is deleted in every template-space folder before mirroring, with or without mirroring and also when the run then fails (for example at the flip transform). A `vat` step that recomputes the native outputs only (no normalization transform, `07`) does not touch the template-space folders and deletes no flipped file. Whenever flipped files were deleted, a note says how many were removed and to run *Calculate* with mirroring again, unless this run is a mirroring run whose step for the patient then completed (every template-space folder was mirrored again). The note is therefore given without mirroring, and whenever the patient's step failed or was cancelled, in the `vat` step, at the flip transform or while mirroring a later template space. A deleted flipped file whose original no longer exists is not written again by a completed mirroring run, and needs none. A flipped file that cannot be deleted (for example held open by another program on Windows) would be read as the mirror of the new original, so the patient's step fails with `E_GROUP_FLIP`, naming each such file and its cause and saying to close the program, then delete the files or run *Calculate* with mirroring (a template-space label folder that exists but cannot be listed counts the same, named by its folder; when the `vat` step was cancelled the step stays cancelled, with the same message); mirroring (`flip::mirror_label_dir`) likewise fails with `E_GROUP_FLIP` when a stale flipped file cannot be deleted. The watch is process-wide: a `vat` run for the same patient and label started elsewhere in the same process while the step runs also counts, which errs towards deleting. A run that recomputes nothing keeps the flipped files. *Calculate* with mirroring then writes them all again (`flip::mirror_label_dir`). A `vat` run outside *Calculate* (e.g. `dbsloc run --stages vat`) does not delete them.
- **Points:** negate x, then map through the inverse composite (`specs/05` §6 point warp).
- **File names:** `sub-<id>_sim-<binary|efield|efieldgauss>_model-<slug>_hemi-R_hemidesc-FlippedFromLeft.nii` and `…_hemi-L_hemidesc-FlippedFromRight.nii` beside the originals (Lead-DBS names; required by its explorers). They are deleted whenever *Calculate* rewrites the originals, with or without mirroring (rule in *Images* above).
- **Extension:** Lead-DBS's explorers match these names only with the `.nii` extension. Every stimulation image, of any label, is written uncompressed `.nii` (`07` §5.1, the exception to the `.nii.gz` rule of `specs/01` §7a), the flipped files included (aligned 2026-10-23 with `07` §5.1).

### 4.5 Locks, invalidation and fingerprints

- Per subject: one lock per subject stage as today (`specs/00` §6). `vat` invalidation follows `specs/00` §6.1 (`reconstruct`, `orientation` → `vat`). A changed stimulation setting invalidates that label's `vat` outputs (the label is no longer up to date, `07` §7.2, §7.6).
- Per group analysis: every analysis record (§7.7, §8.6, §9.5, and the atlas-statistics table) stores `inputs`: SHA-256 of each patient's files it read (template VTA / E-field / flipped files, Reco v2 JSON, statistics JSON `sub-<id>_desc-vtastats.json` (`07` §8.4), seed maps, connectome id), the clinical variable values used (canonical JSON), the settings and the patient selection, plus `software`. A record whose recomputed fingerprint differs is shown **stale** (results kept, *Recompute* offered). No group-level lock files exist.

### 4.6 Desktop, CLI and server

- **Library:** `dbsloc-pipeline` module `group` (record I/O, `M` mapping, batch planning) and a statistics module (§6–§9); front ends never compute statistics themselves.
- **CLI:** `dbsloc group create|add|remove|set-group|clinical import|clinical export|info|calc|stats|correlate|sweetspot|fibrefilter|netmap|view|fliplr build|fliplr show|export|import-lead-group <group file>` (there is no `write-lead-group` subcommand: `export` writes the Lead Group file and the clinical table); JSON summary like `dbsloc run`.
- **Server (`docs/SERVER_CLIENT_PLAN.md`):** one job per **cohort analysis** (`kind` ∈ `group_calc`, `group_stats`, `sweetspot`, `fibrefilter`, `netmap`, `export`), run by one runner subprocess that processes the subjects in sequence; GPU job queue shared with subject runs (`gpu_jobs` slots). Events stream per subject as in batch runs. Artefacts are files under the group folder and subject folders, fetched through the file allowlist; API output never contains absolute server paths. The Lead Group twin written on the server stores `patient.list` entries as `derivatives/leaddbs/sub-<id>` (dataset-relative): opened on a desktop inside `<dataset>/derivatives/leadgroup/<guid>/`, Lead Group's remap (§2.5 step 7) finds the subjects. Clinical variables are uploaded as TSV (§10.3) and are subject to the server's audit log.

---

## 5. Group 3D view

### 5.1 Manifest extension

The scene manifest (`specs/08` §11, `scene_manifest.rs`) gains `manifest_version: 2` with a `group` block; version-1 files keep working. One manifest per group view: `<group folder>/viz/scene_manifest.json`.

| Key | Content |
|-----|---------|
| `space`, `template_nii`, atlas keys, `backdrops` | As version 1 (the group's template space and atlas set). |
| `group.guid` | Group id. |
| `group.subjects[]` | `{subject, label, group, reco_json, block: "mni"|"reference", elmodel, active_contacts: {"1": [bool…], "2": [...]}, vta: {"1": <path or null>, "2": ...}, vta_mesh: {...}, flipped: {...}, value: {"1": [...per contact or per side...], "2": ...}}` |
| `group.groups[]` | `{number, label, color}` |
| `group.colour_by` | `"group"` or `{"clinical": <variable name>, "normalisation": "none"|"zscore"|"van_albada", "colormap": "blue_white_red"|<name>, "range": [min, max]}` |
| `group.mirror` | bool (§5.4) |
| `group.show` | `{active_contacts, passive_contacts, highlight_active, vtas, point_cloud, isovolume}` |

### 5.2 Rendering rules (reference behaviour)

- Leads are drawn in template space from each subject's markers and model (`specs/08` §11 electrode bodies); contacts from `coords_mm` (recomputed from markers).
- **Colour by group:** insulation and contacts take the group colour. **Highlight active contacts:** active contacts red `[0.9 0.1 0.1]`, opaque, with a pale edge `[0.9 0.9 0.7]` and normal size; inactive contacts smaller (half marker size). *Show active* / *show passive* hide the other set.
- **Colour by clinical variable** (`colorpointcloudcheck`): each contact is a point coloured by its value mapped linearly from [min, max] of the shown values onto the colour map; default map blue `[0 0 1]` → white → red `[1 0 0]` (Lead-DBS default; it additionally applies a contrast stretch whose exact form is **unverified**, we map linearly). NaN values are not drawn. A per-patient variable colours all active contacts of the patient (the reference expands it to every contact with a valid coordinate of that side); per-hemisphere uses the side's column; per-contact / per-contact-pair values map to contacts / contact midpoints (§10.1).
- **VTAs:** each side's template-space VTA surface (marching cubes at 0.5 on the binary VTA, or the mesh written by `specs/07`), translucent through the OIT pass (`specs/08` §6: yellow, α 0.35 default) or tinted with the group colour when colouring by group.
- **Legend:** group colours with labels; or a colour bar with the variable name and range.
- **Interaction:** selecting a patient in the list highlights its leads (others dimmed to α 0.3); selecting a group highlights its patients; hovering a lead shows `sub-<id> · group · side · value`. Double click centres on the lead. Camera default and navigation as `specs/08` §11.1.5; the default zoom centre is the mean of all contacts (Lead-DBS centres on the mean of both sides' contact means).

### 5.3 Contact values (`isomatrix`)

A clinical variable becomes per-contact values per side with the rules of Lead-DBS's `ea_reformat_isomatrix.m`:

| Variable shape (N × w) | Values per side |
|------------------------|-----------------|
| w = 1 | every contact of the side gets the patient value |
| w = 2 | right contacts get column 1, left contacts column 2 |
| w = 2·(Kmax − 1) | first half → right contact pairs, second half → left; drawn at contact-pair midpoints |
| w = 2·Kmax | first half → right contacts, second half → left |
| other | error `E_GROUP_ISOMATRIX` |

Kmax = the largest contact count in the group. Contacts beyond a patient's own K are NaN. Normalisation (`normregpopup`) is applied per side over all finite values: z-score (mean and SD of finite values) or van Albada & Robinson 2007 (rank → normal quantile).

### 5.4 Mirroring

With `mirror` on, every patient is duplicated: the copy's side 1 takes the original's side 2 markers and vice versa, each marker is mapped through the non-linear flip (§4.4), contacts and trajectory are re-resolved from the flipped markers, and the copy is named `<name>_mirrored`. Contact values are mirrored the same way (right' = right rows then left rows; left' = left then right). VTAs use the `hemidesc-Flipped…` files.

### 5.5 Performance

- Electrode bodies are instanced per catalogue model (one mesh per model, one transform + colour per lead); contacts as instanced quads or spheres.
- Target: 1000 leads (500 bilateral patients) with 1000 VTA meshes of ≤ 5 000 triangles each, mirroring off, renders at ≥ 30 frames per second on the project's reference integrated adapter (`docs/gpu_vendor_report.md`) and loads in ≤ 10 s from cached meshes. These numbers are targets, not measured (**unverified**).
- VTA meshes are cached per file (keyed by content hash) like atlas meshes (`specs/08` §11 mesh cache).

### 5.6 Optional layers (later in the release)

- **Isovolume:** per side, the contact values (§5.3) are interpolated as scattered data onto a 100³ grid spanning the bounding box of the points; natural-neighbour (Sibson) interpolation without extrapolation (NaN outside the convex hull). Display 1 = point cloud of every 10th finite grid node coloured by value; 2 = Gaussian-smoothed volume, isosurface coloured by value, α 0.7 (`ea_showisovolume.m`; smoothing kernel 15 voxels per the reference, **unverified** sigma). Export as NIfTI on the template grid.
- **Statvat maps** (`ea_exportvatmapping.m`): per voxel over the selected patients, from binary VTAs each multiplied by the patient's value: one-sample t, mean, median and the count N of covering VTAs; for a per-patient variable the left VTA is united with the flipped right VTA; for per-hemisphere variables sides are computed separately and summed; `_nthresh` versions blank voxels with N < 10 % of max N. Superseded by the sweet-spot explorer (§7); kept for Lead Group parity, low priority.

---

## 6. Atlas overlap tables

### 6.1 Cohort VTA table

Built from the per-subject statistics (§3) of the group label; one row per patient × side × structure × VTA index:

| Column | Content |
|--------|---------|
| `subject`, `group`, `group_label`, `side` (`R`/`L`), `source` (j) | identity |
| `structure` | atlas structure name without extension |
| `vta_volume_mm3` | `vat.volume` |
| `overlap_mm3` | `AtlasIntersection` |
| `frac_of_vta` | `nAtlasIntersection` (0–1) |
| `frac_of_structure` | `nWithinAtlasIntersection` (0–1) |
| `structure_volume_mm3` | `AtlasVolume` |
| `efield_sum` | `efield.volume` (Σ E, V/m) or empty |
| `efield_in_structure` | `efield.AtlasIntersection` or empty |
| `frac_of_efield` | `efield.nAtlasIntersection` or empty |
| `amplitude` | source amplitude of that side (from `vat.amp`, the active sources joined with `;`) |

Missing values are empty cells in TSV (§10.5).

### 6.2 Contact distance table

One row per patient × side × contact × structure: `distance_mm` (`conmat`), `inside_hull` (bool from `conmat_inside_hull` = 0), `inside_voxel` (bool from `conmat_inside_vox` = 0), plus `active` (from the stimulation settings). Needs no VTA (PD1 single-patient use).

### 6.3 Summaries

- Per group and side and structure: N with a VTA, mean, SD, median, min, max of `overlap_mm3`, `frac_of_vta`, `frac_of_structure`.
- **Side combination** (Lead-DBS `ea_lg_stats.m`): `both` = right + left (a missing side counts 0); `right` = side 1; `left` = side 2.
- **Correlation with a clinical variable:** Pearson and Spearman r with a permutation p (5 000 label permutations, two-sided; exact enumeration when n ≤ 7) of the chosen measure against the variable; per-patient variable against `both`, `right`, `left`; per-hemisphere variable: `right` vs column 1, `left` vs column 2, `both` stacks right then left (2N points).
- **Two-group test:** when the variable has exactly two distinct finite values and is per patient: Student two-sample t-test (equal variances, MATLAB default) of the measure between the two groups, with a box plot.
- These match the reference's statistics explorer; plots are shown in the GUI and the numbers go into the exports.

### 6.4 Lead-DBS export (`ea_lg_exportstats.m`)

We write the Lead-DBS export `dataset-<dataset>_analysis-<guid>_desc-stats_export.mat` (MAT v7) with the variables `stimulation` (label), `clinicalVars` (C×1 cell), `clinicalVarNames` (C×1 cell), `clinicalVarSelected`, `atlas`, `atlasNames` (structure names without extension), when present `parcellation`, `parcellationNames`, `connectome`, and the N×2 cells `AtlasIntersectionVAT`, `nAtlasIntersectionVAT`, `AtlasIntersectionEfield`, `nAtlasIntersectionEfield`, `fibercounts`, `nfibercounts` (row p = patient, column = side, element = 1×A vector). **Reference defect (not reproduced):** the reference overwrites `fibercounts` with `nfibercounts` and stores only the last patient's fibre counts; we write both correctly.

---

## 7. Sweet-spot mapping

Behaviour of `explorers/sweetspot_explorer/ea_sweetspot.m` and `ea_sweetspot_calcstats.m`, written as mathematics. Papers: Butson 2011, Eisenstein 2014, Dembek 2017, Reich 2019, Dembek 2019, Elias 2021, Treu 2020 (§14).

### 7.1 Inputs

- Patients P (selection, default `M.ui.listselect`), outcome I (one clinical variable, N×1 or N×2), optional covariates (other clinical variables, N×1 or N×2).
- Per patient p and side s ∈ {R, L}: the template-space E-field `sim-efield_model-<slug>_hemi-<s>.nii` of the group label, and its flipped counterpart (§4.4). Rows 1…N are originals, rows N+1…2N the flipped copies (row N+p of side R = patient p's left field flipped to the right).
- **Deviation D6:** Lead-DBS always reads E-field files, even at statistic level *VTAs* (it binarises them at the threshold). Analytical VTA models produce no E-field (`specs/07`); at level *VTAs* we use the binary VTA file when no E-field exists. Level *E-Fields* requires E-fields (`E_SWEETSPOT_NO_EFIELD` lists the patients).
- Settings with defaults:

| Setting | Default | Options |
|---------|---------|---------|
| resolution r | 0.5 mm | > 0 |
| level | `VTAs` | `VTAs`, `E-Fields` |
| E-field threshold θ | 200 V/m | |
| NaN threshold (E-Fields) | 0 V/m | |
| coverage threshold c | 20 % | 0–100 |
| test | `T-Test` | `Mean-Image`, `N-Image`, `T-Test`, `Wilcoxon-Test`, `Proportion Test (Binary Var)`, `Binomial Test (Binary Var)` (level VTAs); `Correlations`, `Reverse T-Tests (Binary Var)` (level E-Fields) |
| null hypothesis H0 | `Zero` | `Zero`, `Average`, `Threshold` |
| improvement threshold | 0 | used by `Threshold` H0 and `N-Image` |
| N threshold | 0 % | `N-Image` |
| amplitude correction | `None` | `None`, `Amplitude`, `VTA Size`, `Amplitude (discrete)`, `Amplitude (regression)` |
| outcome normalisation | `None` | `None`, `z-score`, `van Albada 2007` (the reference labels it "van Albada 2017") |
| correlation type | `Spearman` | `Spearman`, `Pearson`, `Bend` (skipped variants later) |
| show significant only | on | |
| α | 0.05 | |
| multiple comparisons | `Uncorrected` | `Uncorrected`, `FDR`, `Bonferroni` |
| mirror sides | off | |
| split by group | off | |
| show positive / negative | on / off | |
| cross-validation | `Leave-One-Patient-Out` (our default; the reference's initial menu choice is **unverified**) | §7.5 |
| k (k-fold) | 5 | |
| permutations | 1000 | |
| prediction basis | `Mean of Scores` | `Mean of Scores`, `Sum of Scores`, `Peak of Scores`, `Peak 5% of Scores`; level E-Fields also `Profile of Scores: Spearman / Pearson / Bend` |
| random seed | fixed (`rngseed` 'default' in the reference) | integer |

**Literature presets** (the reference's *Inspire analysis by* menu sets these):

| Preset | Test | H0 | Improvement threshold | N threshold | Amplitude correction |
|--------|------|----|----|----|----|
| Butson 2011 | Mean-Image | Threshold | 0.5 | — | None |
| Cheung 2011 | N-Image | Threshold | 0.75 | 75 % | None |
| Eisenstein 2014 | T-Test | Zero | 0 | 0 | None |
| Reich 2019 | T-Test | Average | 0 | 0 | None |
| Dembek 2019 | Wilcoxon-Test | Average | 0 | 0 | Amplitude (discrete) |
| Elias 2020 | Wilcoxon-Test | Average | 0 | 0 | VTA Size |

### 7.2 Common grid

Per side, over all 2N files of that side: the bounding box of voxels with E > 150 V/m (`prefs.explorer.efieldlowfilter`; for binary VTAs: voxels > 0.5), corners in world mm, extended outward to multiples of r (floor of the minimum, ceil of the maximum per axis). The grid is axis-aligned with spacing r, its first voxel centre at the box minimum and its last at the box maximum. Each file is resampled onto it by **nearest neighbour** (reference behaviour; also for E-fields). Values outside a file's own field of view are 0. X_s is the (2N × V) matrix of the side's voxel values.

### 7.3 Outcome preparation

1. If I is N×1, use it for both sides. If mirroring, stack it twice (rows N+1…2N = rows 1…N); covariates likewise. **Deviation D7:** for a per-hemisphere outcome the flipped copy of patient p's left field (row N+p of side R) gets patient p's **left** score; the reference gives it the right score (it stacks the matrix unchanged).
2. Normalisation per side over the stacked vector: z-score (finite values) or van Albada & Robinson 2007: x ↦ Φ⁻¹(rank(x) / (n + 1)) with average ranks for ties (**unverified** that the reference uses n + 1; implement as the paper).
3. Covariates: replace I by the residuals of an ordinary least-squares fit of I on the covariates **with** an intercept (**unverified** whether the reference adds an intercept).
4. Patient set Q = P, plus P + N when mirroring. With *split by group*, the steps of §7.4 run separately for each group's members of Q, giving one map per group (only one sign can be displayed then).

### 7.4 Voxel statistics (one side; v = voxel, i ∈ Q)

Let n = |Q| and n_c = round(n · c / 100).

**Level VTAs.** B_i(v) = 1 if X_i(v) > θ, else 0 (binary VTA input: X_i(v) > 0.5). Coverage N(v) = Σ_i B_i(v). Covered set C(v) = {i : B_i(v) = 1}.

- Amplitude a_i = source-1 amplitude of the side (`S.Rs1.amp` / `S.Ls1.amp`, reference behaviour), size z_i = Σ_v B_i(v) (voxel count; 0 → NaN).
- **H0:** `Zero` → 0; `Threshold` → improvement threshold; `Average` with amplitude correction `None` → mean_i I_i; `Amplitude` → mean_i (I_i / a_i); `VTA Size` → mean_i (I_i / z_i); `Amplitude (discrete)` → per patient h_i = mean of I_j over patients j with a_j = a_i; `Amplitude (regression)` → h_i = b · a_i with b from the least-squares fit I ≈ b·a without intercept. The last two give a per-patient H0.
- **Correction of I:** `Amplitude` → I_i / a_i; `VTA Size` → I_i / z_i; others unchanged.
- **Mean-Image:** M(v) = Σ_{i∈C(v)} I_i / N(v); NaN where N(v) < n_c.
- **N-Image:** among patients with I_i > improvement threshold (all patients if I is binary), count N'(v); NaN where N'(v) < round(n' · Nthr / 100), n' = number of such patients.
- **T-Test:** for voxels with N(v) ≥ n_c: one-sample t-test of {I_i : i ∈ C(v)} against H0 (paired t-test of I_i against h_i for a per-patient H0): t(v) = (mean − H0) / (sd / √N(v)), sd with N − 1; p two-sided. Value = t; Inf → NaN; voxels below coverage → NaN.
- **Wilcoxon-Test:** for voxels with N(v) ≥ n_c: Wilcoxon signed-rank test of {I_i − H0} (or I_i − h_i) over C(v), two-sided p; value = −log10 p, negated when median(I over C(v)) − H0 < 0 (for a per-patient H0: median(I) − median(h)).
- **Proportion Test (Binary Var):** responders R = {I_i > 0}, non-responders R̄; voxels with N(v) > n_c (strict). Compare the proportions k₁/|R| and k₀/|R̄| with k₁ = covered responders, k₀ = covered non-responders, by a χ² test of two proportions with Yates continuity correction; value = the χ² statistic signed by k₁/|R| − k₀/|R̄| (**unverified**: the reference returns the third output of its proportion-test helper, whose meaning was not confirmed); p from χ²(1).
- **Binomial Test (Binary Var):** for voxels with N(v) > n_c: k = covered responders, m = N(v), π₀ = |R| / n; value = k/m − π₀; p = exact two-sided binomial test of k in m against π₀. **Deviation D8:** the reference uses the binomial point probability P(X = k) as the p-value.

**Level E-Fields.** E_i(v) = X_i(v); values below the NaN threshold → NaN. Coverage N(v) = #{i : E_i(v) > θ}; voxels with N(v) < n_c are NaN for all patients.

- **Correlations:** r(v) = correlation (Spearman, Pearson or percentage-bend with β = 0.2, Wilcox 1994) between {E_i(v)} and {I_i} over patients with both finite; voxels with fewer than 3 such pairs → NaN. p from the correlation test.
- **Reverse T-Tests (Binary Var):** voxels with Σ E > 0 and enough finite values (≥ n_c); responders R = {I_i > 0}; if every patient is a responder: one-sample t-test of E over R against 0; else Student two-sample t-test (equal variances) of E over R vs R̄. Value = t.

**Significance (when *show significant only*).** Over the voxels with a finite value, numtests = their count; `Bonferroni`: p' = p · numtests; `FDR`: p' = p · numtests / rank(p) (ascending rank, **without** the monotone step-up minimum — reference behaviour, kept for parity; a later option may add the standard Benjamini–Hochberg adjustment); `Uncorrected`: p' = p. Voxels with p' > α become NaN. N-Image and Mean-Image have no p and are not filtered.

**Output.** vals_s (V × 1, NaN = not part of the map), per group when split. Positive values = sweet spot, negative = sour spot.

### 7.5 Cross-validation and evaluation

| Strategy | Training / test |
|----------|-----------------|
| Leave-Nothing-Out (Permutation-Based) | train = test = all P; repeated with the outcome permuted |
| Leave-One-Patient-Out | n folds, each leaves out one patient (with mirroring: its flipped copy is left out with it) |
| Leave-One-Cohort-Out | one fold per group number (error with one group) |
| k-fold (randomized) | k random folds (seeded) |
| Custom (patients / cohorts / sets) | user-defined partitions; first release: patients and cohorts only |

**Predicted score Ihat** of a test patient j on side s from the training map m = vals_s with only the visible signs kept (positive values set NaN if positives are hidden, negatives if negatives are hidden):

- Level VTAs, B_j(v) as above:
  - Mean of Scores: Σ_v m(v)·B_j(v) / #{v : m(v) finite} — the denominator is the number of finite map voxels, not the VTA size (reference behaviour).
  - Sum of Scores: Σ_v m(v)·B_j(v) over finite m.
  - Peak of Scores: the largest finite m(v)·B_j(v) (positives shown only), the smallest (negatives only), or their sum (both).
  - Peak 5% of Scores: the same with the sum of the top ⌈0.05·V⌉ values (V = voxel count of the grid).
- Level E-Fields: the same four with E_j(v) in place of B_j(v); and Profile of Scores: Fisher z = atanh of the correlation (Spearman / Pearson / bend) across voxels between m and E_j.
- Combination: per-patient outcome → Ihat_j = mean over the two sides (NaN-aware), compared with I_j; per-hemisphere outcome → both sides are separate samples (2n pairs).

**Evaluation:** Spearman r (default; Pearson selectable) between Ihat and I over the test predictions, with its p (the permutation path uses Spearman by default in the reference; the correlation shown for the other strategies is **unverified**). Leave-Nothing-Out: compute r₁ on the real outcome and r_k (k = 1…1000) with the outcome permuted among P (seeded); p_perm = fraction of |r_k| ≥ |r₁|; report also the 95th-percentile |r| of the null. Binary outcomes additionally report the area under the ROC curve of a logistic regression of I on Ihat (in-sample for Leave-Nothing-Out, fitted on training folds otherwise).

### 7.6 Prediction of a new patient (single-patient support, PD1)

A **sweet-spot model** = the map computed on all selected patients, the grid, the settings and a linear model I ≈ β₀ + β₁·Ihat fitted (ordinary least squares) on the Leave-Nothing-Out Ihat of the same patients. For a new patient (any subject with a template-space VTA / E-field under any label): Ihat from §7.5, predicted outcome β₀ + β₁·Ihat, reported with the training r and n and the warning that it is a research estimate. A model can be applied to another group (*Import model*), which requires the same template space and level.

### 7.7 Outputs

Under `<group folder>/sweetspots/<id>/` (`<id>` = `<guid>`, `<guid>_1`, … like the reference):

| File | Content |
|------|---------|
| `sweetspot.json` | analysis record: settings, patient selection, outcome name, `inputs` fingerprint (§4.5), cross-validation results (per patient I, Ihat; r, p, p_perm, AUC), model β₀, β₁, `software` |
| `dataset-<d>_analysis-<guid>_sweetspot-<id>_hemi-<R|L>_stat-<test>_desc-<pos|neg|both>_map.nii.gz` | the map (float32, NaN outside), one per side (and `_group-<g>` per group when split) |
| `…_desc-coverage_map.nii.gz` | N(v) |
| `model.json` + the maps | the stored model (§7.6) |

**Colours:** positive map from base white `[1 1 1]` to peak `[0.9176 0.2 0.1373]`, negative from peak `[0.2824 0.6157 0.9725]` to white; with both signs the two gradients are joined at 0. Drawn as smoothed isosurfaces of the positive and negative parts (the reference smooths with 10 iterations and simplifies the hull by 0.5) or as coloured voxels on the slices.

### 7.8 Acceptance tests

1. **Planted sweet spot.** Seeded synthetic cohort: 40 patients, binary sphere VTAs (radius 2.0–3.5 mm) with centres uniform in a 12 mm cube; hidden sphere S* (radius 2 mm); I_p = 50 + 30·[VTA_p ∩ S* ≠ ∅] + N(0, 5²). T-Test, H0 Zero → no significant voxels with *Uncorrected* α 0.05 is a failure; H0 Average → the voxel of maximum t lies within 1.5 mm of S*'s centre; Leave-One-Patient-Out Spearman r > 0.5; Leave-Nothing-Out p_perm < 0.05 with 1000 permutations.
2. **Null cohort.** Same VTAs, I independent of position: Leave-Nothing-Out p_perm ≥ 0.05 for at least 18 of 20 seeds.
3. **Hand-computed voxel.** 5 patients, one voxel covered by 3 of them with I = {10, 20, 30}, H0 = 0: t = 20 / (10/√3) = 3.464; Mean-Image = 20; N-Image = 3; with c = 80 % (n_c = 4) the voxel is NaN.
4. **Coverage rounding.** n = 10, c = 25 % → n_c = round(2.5) = 3 (round half away from zero, as MATLAB `round`).
5. **Mirroring.** With mirroring, X has 2N rows, the stacked outcome has 2N entries, and LOO leaves out both rows of a patient.
6. **Grid.** Two fields whose supra-150 V/m boxes are [−10.2, −8.1] and [−9.0, −6.0] in x at r = 0.5 → grid x from −10.5 to −6.0, 10 voxels.
7. **FDR parity.** p = [0.01, 0.04, 0.03] → p' = [0.03, 0.04, 0.045].
8. **Presets** set the fields of §7.1's table.
9. **Determinism.** Same inputs and seed give byte-identical maps and records on CPU and GPU paths.

---

## 8. Fibre filtering (group-level contract)

Connectome file formats, fibre storage and the voxel → fibre mapping primitives belong to `specs/15` (§3, §7; aligned 2026-10-23). This section fixes what the group feature consumes and produces. Provenance: `explorers/fiberfiltering_explorer/ea_disctract.m`, `ea_discfibers_calcvals.m`, `ea_discfibers_calcstats.m`, `ea_disctract_crossval.m`, `ea_compute_fibscore_model.m`, `explorers/stattests/*`. Papers: Baldermann 2019, Li 2020, Irmen 2020, Li 2021, Treu 2020.

### 8.1 Inputs

- Patients, outcome and covariates as §7.1; the same 2N-row layout of original and flipped volumes (§4.4) per side. The flipped row of side s is the mirror of the patient's *other* lead (`hemi-<s>_hemidesc-FlippedFrom<other side>`) and is present whenever that lead's original exists, also for a patient with only one lead: a left-only patient gives its original to side L and its flipped copy (with the left outcome, D7) to side R (corrected 2026-11-12; before, the flipped row of side s was looked up only when the patient also had an original on side s). Within each side a patient's original row comes before its flipped row. A flipped file that is missing or not on the mirrored grid of its original (§4.4) is left out with a note asking for *Calculate* with mirroring again.
- Per patient and side: the template-space E-field (or binary VTA for the VTA model) of the group label.
- A structural connectome in template space from `specs/15`: fibres as point sequences in mm with a per-fibre id; read-only, never bundled (licences and access classes in `specs/15` §2.3).

### 8.2 Per-fibre values per patient (voxel-based method)

The values are those of `specs/15` §7.2, which is normative where this summary is shorter (aligned 2026-10-23). For each side and volume i:

1. Supra-threshold voxels: |E| > calc threshold (default 200 V/m; for a binary VTA, the non-zero voxels).
2. Keep the fibre points inside the bounding box of those voxels' centres (inclusive); map each kept point to the nearest voxel (inverse affine, rounding); a fibre with any kept point outside the volume's grid is dropped; repeated voxels of one fibre count once.
3. A fibre is **connected** if any of its voxels is supra-threshold. Over the fibre's supra-threshold voxels: binary (1 if connected), sum, mean, peak (max), peak 5 % (mean of the top ⌈0.05·n⌉ values). Unconnected fibres get 0.
4. Only fibres connected to at least one volume of the side are kept (per side); the result is a sparse (fibres × 2N) matrix per value type. A side whose amplitudes are all 0 contributes nothing.

Optional **sigmoid field** transform s(E) = 1 / (1 + exp(−k(E − x₀))) with x₀ = 206 V/m and k = 0.0203 per V/m (midpoint and slope from the 61 and 351 V/m thresholds of Åström 2015, as in the reference).

### 8.3 Statistics per fibre

1. Choose the value matrix: binary (VTA model) or the E-field metric (default `Peak`).
2. Regress covariates out of the outcome (§7.3).
3. Coverage: N_f = number of selected patients with the fibre connected (VTA model) or with value > E-field threshold (default 200 V/m). The fibre is excluded if N_f < (connthreshold / 100) · n (default 20 %); for the VTA model also if N_f > (1 − connthreshold / 100) · n.
4. Test per fibre (tests carry name, family, outcome type and compatible models; default family `Correlations`, test `Spearman`, H0 `Average`):

| Test | Outcome | Model | Fibre value |
|------|---------|-------|-------------|
| Spearman / Pearson / Percentage Bend correlation | gradual | E-field, sigmoid | r between fibre values and outcome (pairwise complete) — Irmen 2020 / Li 2020 "E-field" mode |
| Two-sample t-test | gradual | VTA | t of outcomes of connected vs unconnected patients — Baldermann 2019 "VTA" mode |
| One-sample t-test | gradual / binary | VTA | t of connected patients' outcomes vs H0 |
| Wilcoxon rank-sum | gradual | VTA | sign(z)·(−log10 p) |
| Wilcoxon signed-rank | gradual | VTA | sign(median − H0)·(−log10 p) |
| Reverse two-sample t-test | binary | E-field, sigmoid | t of fibre values, responders vs non-responders |
| Proportion test | binary | VTA | χ² proportion test, continuity-corrected |
| Weighted regression (one- / two-sample) | gradual | E-field, sigmoid | weighted least-squares t of the group coefficient, weights = fibre values (min–max scaled to [0, 1] when outside it), df = 2n − 2 |
| N-map / Mean-map | any | any | number of connected patients / mean outcome of connected patients (descriptive, no p) |

5. Significance as §7.4 (pooled over sides and groups; defaults: off, FDR, α 0.05).
6. **Display amount:** positive and negative fibres are thresholded separately; default strategy *Percentage Relative to Peak* with 25 % per side: keep fibres with value ≥ max − 0.25·(max − min) of the positive values (pooled over sides), symmetric for negatives; other strategies: percentage of the fibre count, fixed count per side, empirical-CDF quantile, fixed value. Positives shown, negatives hidden by default. Colours: positive `[0.9176 0.2 0.1373]`, negative `[0.2824 0.6157 0.9725]`, white at the low end, 1024 levels.

### 8.4 Cross-validation and prediction

Strategies as §7.5 (plus repeated k-fold `kIter`, default 1). Per test patient: Ihat = mean (default), sum, peak or peak 5 % over the training model's shown fibres of (fibre score × the patient's fibre value); for a per-patient outcome both sides' fibres are pooled into one score; a patient touching none of the shown fibres gets NaN. Evaluation and permutation test as §7.5 (correlation type Spearman, NaN r counted as 1e−5 in the reference). Optional fit to actual scores: linear model of I on Ihat (and covariates) on the training patients.

### 8.5 New-patient prediction

A stored model = fibre scores (per connectome fibre id, zero where unscored), connectome id, value type, settings and the linear model of §8.4 fitted on all selected patients; applying it to a new patient needs the same connectome and value type.

### 8.6 Outputs

`<group folder>/fiberfiltering/<id>/`: `fibrefilter.json` (settings, `inputs`, cross-validation results, model), `fibres_<R|L>.mat` (the shown fibres with their scores as `vals`, in the fibre subset format of `specs/15` §7.4; aligned 2026-10-23, `15` defines no other fibre layout), optional exports `.trk` and a NIfTI density (each shown fibre adds its score once to every voxel it passes, positives and negatives in separate files, on the template T1 grid). The 3D view draws the shown fibres as coloured line strips.

---

## 9. Network mapping (group-level contract)

Seed maps are produced by the connectivity jobs of `specs/15` (§5, §6, §8, §9; aligned 2026-10-23). Provenance: `explorers/networkmapping_explorer/ea_networkmapping.m`, `ea_networkmapping_calcstats.m`, `ea_ihat_databaselookup_netmap.m`. Papers: Horn 2017, Boes 2015, Li 2021.

### 9.1 Inputs

- Patients, outcome (a per-hemisphere outcome is averaged to one value per patient), covariates (regressed out as §7.3).
- One seed map per patient (both hemispheres in one seed) and, with mirroring, one per flipped seed (rows N+1…2N, same outcome): the structural map `sub-<id>_sim-<kind>_model-<slug>_seed-dMRI_conn-<tag>_strucmap.nii` (fibre density through the seed) or the functional map `sub-<id>_sim-<kind>_model-<slug>_seed-fMRI_conn-<label>_desc-AvgRFz_funcmap.nii` (seed-to-voxel correlation, Fisher-z per connectome subject, averaged), in the template-space label folder `stimulations/<space>/gs_<guid>/` (`15` §9.2: `<tag>` = connectome tag, `<label>` = tag + subset; `<slug>` per `07` §1.1). Flipped seeds are `…_seed-<dMRI|fMRI>_hemidesc-flipped.nii` (`15` §8). Aligned 2026-10-23. All maps of one analysis share the mapper's output grid (structural default 2 mm, 91 × 109 × 91; functional = the connectome's own grid). No resampling in the group step.

### 9.2 Maps

X = (patients × voxels). For each voxel v:

| Map | Definition |
|-----|-----------|
| R-map (default, Horn 2017) | r(v) = Spearman (default), Pearson or percentage-bend correlation between X(·, v) and I across patients; pairwise complete; no further Fisher transform |
| A-map (weighted average) | Σ_p X(p, v) · I_p (not divided by n, reference behaviour) |
| C-map (combined) | A-map where sign(R) = sign(A), NaN elsewhere |
| N-map (Boes 2015) | #{p : X(p, v) > t} − #{p : X(p, v) < −t}, t = 0.2 by default, each count only for a shown sign |
| One-sample t | t over patients |
| Two-sample t | Student t between two selections |

Significance as §7.4 (default off; FDR default, α 0.05). Positive part shown by default.

### 9.3 Cross-validation

Strategies as §7.5 (k-fold with `kIter` repeats). Prediction of a test patient: **similarity to the training map** = Spearman (default), Pearson or bend correlation across voxels **inside the mask** between the training map and the patient's seed map (raw map). Masks: grey matter (default), brain, cortex + cerebellum, cortex, cerebellum (template masks at the map's resolution; their files are not specified yet: `specs/15` defines only a user `--output-mask`, open item noted 2026-10-23), or a user NIfTI. Evaluation as §7.5.

### 9.4 New-patient prediction

Model = R-map (or chosen map), mask, connectome id, correlation type and the linear model I ≈ β₀ + β₁·similarity fitted on all selected patients. A new patient needs its seed map from the same connectome.

### 9.5 Outputs

`<group folder>/networkmapping/<id>/`: `netmap.json` (settings, `inputs`, results, model), `…_netmap-<id>_stat-<rmap|amap|cmap|nmap|t>_map.nii.gz`, optional per-fold model maps when requested.

---

## 10. Clinical variables

### 10.1 Definition

A clinical variable is a name and an N × w matrix of `double` aligned with the patient order; NaN = missing. Shapes (Lead-DBS `ea_edit_regressor.m`):

| w | Kind | Columns |
|---|------|---------|
| 1 | one value per patient | value |
| 2 | one value per hemisphere | right, left |
| 2·K | one value per contact | k1R … kKR, k1L … kKL (the editor offers 8 columns, K = 4, **unverified** for directed leads) |
| 2·(K − 1) | one value per contact pair | K1-2R … , K1-2L … (editor: 6 columns) |

Our record adds metadata that `M` has no place for (kept in JSON only):

```text
{"name": "UPDRS3_improvement_pct", "kind": "per_patient|per_hemisphere|per_contact|per_contact_pair",
 "values": [[...], ...],                          // N rows, null = missing
 "measure": "improvement_percent|improvement_absolute|raw_score|binary|covariate|other",
 "higher_is_better": true, "unit": "%", "description": "", "source": "manual|tsv:<file>|lead_group|clinical_scores"}
```

- **Improvement vs raw score.** Statistics use the values as stored. Lead-DBS's analyses assume larger = better (positive maps are "sweet"); the GUI warns when the chosen outcome has `higher_is_better = false` and offers *Use negated values* (stored as a new variable, never in place). Percent improvement convention: (baseline − follow-up) / baseline × 100 for scales where lower is better (UPDRS, YBOCS); the importer can compute it from two columns.
- **Binary variables** (two finite values 0 / 1) enable the binary-outcome tests.

### 10.2 Lead-DBS per-patient clinical scores

Lead Group also fills variables from `<subject>/clinical/clinical_scores.mat` with score definitions in `<leaddbs>/clinical/ea_scores.mat`: baseline and post-operative scores (post-operative entry keyed by the group guid) produce variables named `<score>_<somatotopy>_<factor>_<absolute|percent|cleaned>`. The file structure is **unverified** (MATLAB `table` objects, not decodable outside MATLAB). First release: not read; variables already in `M.clinical` are imported as they are.

### 10.3 Import from a table

- TSV or CSV (delimiter from the extension, UTF-8, optional BOM), first row = header. One column identifies the subject: `participant_id`, `subject`, `sub` or the first column; values `sub-01` and `01` both match.
- Every other column is a variable (per patient). Columns named `<var>_R` / `<var>_L` (or `<var> right` / `<var> left`) pair into a per-hemisphere variable; `<var>_k<n>R` / `<var>_k<n>L` into per-contact.
- Missing: empty, `NA`, `NaN`, `n/a` (case-insensitive). Numbers with `.` decimal separator; a `,` decimal separator is accepted only when the delimiter is a tab.
- Rows for unknown subjects are reported and skipped; group patients without a row get NaN. Existing variables with the same name are replaced only after confirmation (CLI: `--replace`).
- A `participants.tsv` of the BIDS dataset can be offered as a source.

### 10.4 Storage

- JSON group record (`clinical[]`, primary) and the Lead Group twin (`M.clinical.vars` 1×C cell of N×w `double` with NaN for missing; `M.clinical.labels` 1×C cell of names). Reordering or removing patients reorders / removes rows in every variable (Lead-DBS does the same).
- Names are kept verbatim in `M`; in exports they are used as column headers (tabs and newlines replaced by spaces).

### 10.5 Export

`<group folder>/exports/dataset-<d>_analysis-<guid>_desc-clinical.tsv`: columns `participant_id`, `group`, `group_label`, then the variables (per-hemisphere as `<name>_R`, `<name>_L`; per-contact as `<name>_k<n>R` …), empty cell for missing; a JSON sidecar `…_desc-clinical.json` with the metadata of §10.1 (BIDS phenotype style).

### 10.6 Data protection

Clinical scores next to imaging derivatives are personal health data under most data-protection laws. The group record and its `.mat` twin contain them, so they inherit the access rules of the dataset folder; the software keeps no copy elsewhere (no cache, no telemetry), writes subject ids only (never names or dates of birth), and the server applies its audit log and path sanitiser to clinical uploads and exports. Whether outcome data may be stored with the derivatives, and who may access a shared group folder, is the responsibility of the data controller (institution / study protocol); this is not legal advice.

---

## 11. Exports and file naming

All group outputs live under the group folder; subject-level outputs under the subject's derivative folder (`specs/01` §7a).

| Output | Path (relative to the group folder) |
|--------|-------------------------------------|
| Group record | `dataset-<d>_analysis-<guid>.json` |
| Lead Group twin | `dataset-<d>_analysis-<guid>.mat` |
| Twin backup | `dataset-<d>_analysis-<guid>_desc-backup<UTC>.mat` |
| Lead-DBS statistics export | `dataset-<d>_analysis-<guid>_desc-stats_export.mat` (§6.4) |
| VTA atlas table | `exports/dataset-<d>_analysis-<guid>_desc-atlasoverlap_stats.tsv` (+ `.csv` on request) |
| Contact distance table | `exports/dataset-<d>_analysis-<guid>_desc-contactdistance_stats.tsv` |
| Group summaries | `exports/dataset-<d>_analysis-<guid>_desc-atlasoverlapsummary_stats.tsv` |
| Clinical table | `exports/dataset-<d>_analysis-<guid>_desc-clinical.tsv` + `.json` |
| Sweet-spot / fibre filtering / network mapping | `sweetspots/<id>/`, `fiberfiltering/<id>/`, `networkmapping/<id>/` (§7.7, §8.6, §9.5) |
| Scene manifest | `viz/scene_manifest.json` |
| Screenshots | `exports/dataset-<d>_analysis-<guid>_desc-<view name>_screenshot.png` (off-screen render at the requested size, default 2400 × 1800, transparent background optional) |

TSV: tab-separated, UTF-8 without BOM, `\n` line ends, header row, numbers with `.` and up to 17 significant digits, empty cell for missing, sides `R` / `L`. CSV: same content, comma-separated, RFC 4180 quoting. Exports contain no comment lines; provenance is in the JSON sidecar `<name>.json` (`software`, group guid, `inputs`, creation time).

---

## 11a. Desktop explorers (amended 2026-10-25)

The *Group analysis* panel of the desktop GUI runs the three explorers of §7–§9 on a worker thread (progress, *Cancel*), with the owner decisions of 2026-10-25:

- **Placement.** One *Analyses* section below *Clinical variables*, with a kind selector *Sweet spot* / *Fibre filtering* / *Network mapping*, the inputs, *Run*, and a results list of the record's `analyses` of that kind (newest first: id, date, variable, n, r, p, AUC when present, *Show in 3D*, *Open folder*, *Delete*). A run registers its analysis in `analyses` and saves the record, as the CLI does.
- **Settings.** Visible: clinical *Variable* (from §10), *Covariates* (multi-select, optional), *Connectome* (fibre filtering, network mapping; installed connectomes of the matching class, last choice remembered per user), *Test* / *Map* / *Correlation*, *Cross-validation* (§7.5 schemes; default leave-one-out) and the display threshold (*Display percent*, fibre filtering). Every other setting of `SweetspotSettings`, `FibreSettings`, `NetmapSettings` sits in a collapsed *Advanced* section with the CLI defaults; sweet spot also offers the §7 presets as the first control.
- **Network mapping maps.** *Run* first computes, per included patient, the per-subject maps of `15` §8 for `gs_<guid>` with the chosen connectome when they are missing, when their sidecar differs (seed kind, grid, connectome digest) or when an input file the sidecar lists has changed or is gone; a map without a sidecar (for example one made by Lead-DBS) is used as it is and the run notes how many such maps were used, loading the connectome once for all patients, then runs §9. Progress shows both phases.
- **Display.** *Show in 3D* loads the group scene (§5) if needed and adds the result: fibre filtering = the shown fibres of `fibres_R.mat` / `fibres_L.mat` as a fibre layer coloured by score (§8.3 colours); sweet spot and network mapping = the result map as a map layer (`specs/08` §11.1.8). The viewer default for maps is the reference's: smoothed isosurfaces of the positive and negative parts (§7.7 colours), switchable per map to coloured voxels on the slices.
- **Kinds.** The record's `kind` strings are the folder names `sweetspot`, `fiberfiltering`, `networkmapping` (the reference's folders; aligned 2026-10-25 with the code).

---

## 12. Acceptance criteria and validation

1. **Read Lead Group files.** Fixture `M` files written by Lead-DBS 3.2.1 (v7.3) and by an older version (v7, numeric `atlassetpopup`, no `mirrorsides`, `S` without `ver`) open; patient list, groups, colours, clinical variables and `S` match the file; missing fields follow §2.5. A fixture must be produced with MATLAB (Phase D; comparing files is allowed).
2. **Round trip.** Read → group record → write twin → read twin: patient list, groups, colours, clinical variables (bitwise doubles, NaN preserved), `S`, `vatmodel`, `guid`, the modelled `ui` fields and `passthrough` fields equal the original. `dropped_fields` lists exactly the undecodable fields.
3. **Lead Group reads our twin** (Phase D, needs MATLAB): Lead Group opens the folder without error, lists all patients with their groups and colours, shows the clinical variables, the 3D view draws every lead at the same position as ours (≤ 1e−6 mm for markers), and its *Calculate* runs. After Lead Group saves (v7.3), our software imports its changes (§2.7).
4. **`ea_stats` twin.** Our `sub-<id>_desc-stats.mat` loads in Lead Group (Phase D); entries of other stimulation labels are preserved bit-identically; our JSON and MAT carry the same numbers.
5. **Overlap numbers.** Synthetic: a binary sphere VTA of radius 3 mm on a 0.2 mm grid centred in a binary cube atlas of side 4 mm on a 0.5 mm grid, both at the same centre: `nWithinAtlasIntersection` = 1 within 2 %, `AtlasIntersection` ≈ cube ∩ sphere analytic volume within 3 %; hemisphere split removes an atlas copy mirrored to the other side; E-field overlap with a constant field equals constant × covered voxels.
6. **Statistics.** §7.8 for sweet spots; for fibre filtering a planted-tract cohort (fibres through a hidden region; outcome = 1 when a VTA touches them) yields that tract among the top-scored fibres and LOO r > 0.5; for network mapping, seed maps built as noise + I_p·pattern give an R-map correlating > 0.9 with the pattern.
7. **Batch.** *Calculate* on 3 patients where patient 2 has no reconstruction: patients 1 and 3 complete, patient 2 is reported, exit code 1 (CLI), summary lists the reason.
8. **Mirroring.** Flipping a VTA twice returns it within one voxel of the original (Dice ≥ 0.9); a flipped marker set maps x → approximately −x (|x + x'| ≤ 2 mm on the template, **unverified** tolerance).
9. **Server.** The same group job via the API produces byte-identical outputs to the CLI on the same machine; no API response contains an absolute server path; the server twin uses dataset-relative patient paths.
10. **Performance.** Atlas statistics for 100 patients × 2 sides × 30 structures finish in ≤ 2 min on a desktop CPU; a sweet-spot T-test map with 200 patients, 0.5 mm grid (≈ 10⁶ voxels) in ≤ 30 s; LOO for 200 patients in ≤ 10 min (targets, **unverified**).

---

## 13. Edge cases

| Case | Behaviour |
|------|-----------|
| Patient without reconstruction | Shown greyed in the list with "no reconstruction"; excluded from view and statistics; `elstruct` entry written with empty sides. |
| Patient without VTA for the label | Excluded from VTA statistics and explorers (reported per analysis); contact distances still computed. |
| Unilateral lead | The absent side has empty coordinates; its VTA rows are empty (all-zero volume): it contributes no coverage and Ihat for that side is NaN; per-hemisphere outcomes for that side are ignored. |
| Missing clinical value | NaN; the patient is left out of every test that uses that variable (pairwise), counted in n only where present. |
| Different electrode models in one cohort | Allowed (Lead-DBS uses each patient's model unless `elmodelselect` forces one). Contact-shaped variables use Kmax (§5.3); per-contact variables for a model with fewer contacts are NaN beyond K. Directed and ring contacts are mixed only in per-patient / per-hemisphere statistics. |
| Different template spaces | §4.3: excluded from voxel statistics unless bridged; the view uses the `reference` block when the group space is `MNI152NLin2009bAsym`. |
| Different VTA models | Not allowed within one group label (one model per group); `E_GROUP_MODEL_MIX`. |
| Group numbers not contiguous | Renumbered at write (§2.2.3). |
| Patient listed twice | Second entry refused at add; in an imported file: `W_LG_DUPLICATE`, both kept but statistics use the first. |
| Patients from several datasets | Allowed; each patient records its dataset; the twin's absolute paths point to each dataset; Lead Group's remap (first patient only) may not find them if the group folder moves (`W_LG_MULTI_DATASET` at write). |
| Amplitude 0 on a side | VTA statistics 0 (§3.3 step 8); excluded from sweet-spot coverage (empty volume). |
| E-field outside the common grid | Values outside are dropped by the resampling; the grid is built from every file so this happens only for values below 150 V/m. |
| All outcome values equal | Correlation tests give NaN maps; reported "outcome has no variance". |
| Fewer than 3 patients after selection | Statistics refused (`E_GROUP_TOO_FEW`); the view still works. |
| Clinical variable with wrong row count (imported) | Kept in passthrough, not offered (§2.5). |
| Group file edited by Lead Group while open here | Detected at next open or save by hash (§2.7); never silently overwritten. |
| Flip transform missing | Built on first use (minutes, cancellable) or imported; mirroring disabled until present. |

**Codes of this spec** (listed 2026-10-23; no overlap with `E_VAT_*` of `07` §7.4 or `E_CONN_*` / `E_SEED_EMPTY` of `15` §11): errors `E_LG_NOT_GROUP`, `E_GROUP_ISOMATRIX`, `E_GROUP_MODEL_MIX`, `E_GROUP_TOO_FEW`, `E_SWEETSPOT_NO_EFIELD`; warnings `W_LG_GROUPS`, `W_LG_CLINICAL_ROWS`, `W_LG_PATIENT_MISSING`, `W_LG_STATS_STALE`, `W_LG_NAME`, `W_LG_STATS_OMITTED`, `W_LG_DUPLICATE`, `W_LG_MULTI_DATASET`, `W_GROUP_SPACE`, `W_STATS_MAT_SKIPPED`; also `E_GROUP_FLIP` (the flip transform cannot be built or applied, §4.4) and `E_GROUP_USAGE` (reported by the group analyses for a group input error that has no code of its own). Subject stages run by a group job report their own codes (`E_VAT_*`); connectivity jobs report `15`'s.

---

## 14. References

- Horn A, Reich M, Vorwerk J, et al. Connectivity predicts deep brain stimulation outcome in Parkinson disease. *Ann Neurol* 2017;82:67–78. doi:10.1002/ana.24974 (**unverified** DOI).
- Treu S, Strange B, Oxenford S, et al. Deep brain stimulation: Imaging on a group level. *NeuroImage* 2020;219:117018. doi:10.1016/j.neuroimage.2020.117018.
- Dembek TA, Barbe MT, Åström M, et al. Probabilistic mapping of deep brain stimulation effects in essential tremor. *NeuroImage Clin* 2017;13:164–173. doi:10.1016/j.nicl.2016.11.019 (pages **unverified**).
- Dembek TA, Roediger J, Horn A, et al. Probabilistic sweet spots predict motor outcome for deep brain stimulation in Parkinson disease. *Ann Neurol* 2019;86:527–538. doi:10.1002/ana.25567.
- Butson CR, Cooper SE, Henderson JM, Wolgamuth B, McIntyre CC. Probabilistic analysis of activation volumes generated during deep brain stimulation. *NeuroImage* 2011;54:2096–2104. doi:10.1016/j.neuroimage.2010.10.059.
- Eisenstein SA, Koller JM, Black KD, et al. Functional anatomy of subthalamic nucleus stimulation in Parkinson disease. *Ann Neurol* 2014;76:279–295. doi:10.1002/ana.24204.
- Reich MM, Horn A, Lange F, et al. Probabilistic mapping of the antidystonic effect of pallidal neurostimulation: a multicentre imaging study. *Brain* 2019;142:1386–1398. doi:10.1093/brain/awz046.
- Elias GJB, Boutet A, Joel SE, et al. Probabilistic mapping of deep brain stimulation: insights from 15 years of therapy. *Ann Neurol* 2021;89:426–443. doi:10.1002/ana.25975 (the reference's preset is labelled "Elias 2020").
- Cheung T, et al. 2011 (preset "Cheung 2011"; exact paper **unverified**).
- Baldermann JC, Melzer C, Zapf A, et al. Connectivity profile predictive of effective deep brain stimulation in obsessive-compulsive disorder. *Biol Psychiatry* 2019;85:735–743. doi:10.1016/j.biopsych.2018.12.019 (pages **unverified**).
- Li N, Baldermann JC, Kibleur A, et al. A unified connectomic target for deep brain stimulation in obsessive-compulsive disorder. *Nat Commun* 2020;11:3364. doi:10.1038/s41467-020-16734-3.
- Irmen F, Horn A, Mosley P, et al. Left prefrontal connectivity links subthalamic stimulation with depressive symptoms. *Ann Neurol* 2020;87:962–975. doi:10.1002/ana.25734 (pages **unverified**).
- Li N, Hollunder B, Baldermann JC, et al. A unified functional network target for deep brain stimulation in obsessive-compulsive disorder. *Biol Psychiatry* 2021. doi:10.1016/j.biopsych.2021.04.006.
- Boes AD, Prasad S, Liu H, et al. Network localization of neurological symptoms from focal brain lesions. *Brain* 2015;138:3061–3075 (**unverified**).
- van Albada SJ, Robinson PA. Transformation of arbitrary distributions to the normal distribution with application to EEG test-retest reliability. *J Neurosci Methods* 2007;161:205–211. doi:10.1016/j.jneumeth.2006.11.004.
- Wilcox RR. The percentage bend correlation coefficient. *Psychometrika* 1994;59:601–616 (**unverified** pages).
- Åström M, Diczfalusy E, Martens H, Wårdell K. Relationship between neural activation and electric field distribution during deep brain stimulation. *IEEE Trans Biomed Eng* 2015;62:664–672 (**unverified**; source of the 61 / 351 V/m sigmoid thresholds as used by the reference).
- Lead-DBS 3.2.1 (GPL-3), git `5b1008d` — reference behaviour only; read-only.

Cross-references: `specs/00` §5.2, §6 (batch, locks), `specs/01` §7a (derivative names), `specs/04` §5, §5.4a (SyN engine, template bridge), `specs/05` §6, §8 (Reco v2), `specs/07` (stimulation settings, VTA / E-field outputs, statistics writer; aligned 2026-10-23), `specs/08` §11 (scene manifest, meshes, navigation), `specs/09` (electrode catalogue), `specs/11` (atlas sets), `specs/12` (MAT conventions), `specs/13` Part 2 (batch), `specs/15` (connectomes, seed maps, fibre storage; aligned 2026-10-23), `docs/SERVER_CLIENT_PLAN.md`; owner decisions §15.

**Decisions taken in this spec (for the orchestrator):** D1 twin written as MAT v7, JSON primary; D2 group folder independent of `output_dir`; D3 groups renumbered 1…G at write; D4 `stats` derived, never authoritative; D5 own SyN flip transform, Lead-DBS one importable; D6 binary VTA allowed at sweet-spot level *VTAs*; D7 flipped copies get their source hemisphere's score; D8 exact binomial p; D9 FDR kept in the reference form; D10 server twin with dataset-relative patient paths; D11 explorer objects (`.sweetspot`, `.fibfilt`, `.netmap`) neither read nor written; D12 stimulation NIfTIs uncompressed `.nii` (aligned 2026-10-23: `07` §5.1 applies it to every label).

### Implementation notes (Phase C, 2026-10-23; normative where this spec left a choice open)

- **§2.3 / §4.2 group stimulation.** The record stores no `S`. The group's stimulation is the per-subject canonical file `stimulations/native/gs_<guid>/sub-<id>_desc-stimulation.json` (`specs/07` §2.3); *Calculate* seeds a missing one from another label with `--copy-settings-from <label>` (GUI: *Settings from*, see the amendment of 2026-10-25 in §4.2), otherwise the subject fails with phase `not_run`. The twin's `S` is written only when every patient has settings under `gs_<guid>`.
- **§2.2 / §2.7.** `M` carries no group labels, so a twin edit can only renumber groups; labels live in the JSON record only. `import_twin_edits` maps twin group ranks back to our numbers so our own twin never looks edited. Unresolved numeric `atlassetpopup` / `labelpopup` / `connectomename` indices are kept as `"#n"` and written back as numbers. `M.stats` and `M.elstruct` are never decoded on read (`M.stats` is not imported, but when present it is compared with the record on read, rule 9 `W_LG_STATS_STALE`: different patient count, `patname` of an entry differs from the patient at that position, no `stimulation` entry for `gs_<guid>`, or `atlases.names` differing from `M.vilist` or from the first entry; missing fields never count as a difference and the read never fails; not tested against a real v7.3 file).
- **§2.5.** MAT v5 / v7 files with int, single, char-matrix, sparse, object or opaque fields open through `fastwarp_matfile::read_any`; undecodable leaves become `Unsupported`, listed in `lead_group.dropped_fields` with paths such as `M.a.b`, `M.c{2}`, `M.s(3).f`. v7.3 reading is implemented but untested (no fixture).
- **§3.4.** The contact-distance pass rewrites an existing `ea_stats` as MAT v7, dropping fields the typed reader does not decode, and skips files with undecoded values (`W_STATS_MAT_SKIPPED`). `inside_hull` is approximated as "contact inside the thresholded voxel mask". A subject's statistics are recomputed when its reconstruction is newer.
- **§4.1 / §4.5 / §4.6.** `run_calculate` runs per patient in record order: contact distances → `vat` with `vat_labels = [gs_<guid>]` and `vat_stats_atlas` = the group's atlas set → flip when mirroring. The analysis record is `atlasstats/calc/analysis.json`; the group fingerprint hashes the files of included patients. `--subjects` keeps record order. `stop-on-first-failure` marks the rest `cancelled`, exit 1; an external cancel exits 130. The group VTA model falls back to the first subject's when the record has none. CLI syntax: `docs/RUN_REFERENCE.md`.
- **§4.4 mirroring (implemented 2026-10-23, `group/fliplr.rs` + `group/flip.rs`).** The transform is built by `fliplr::resolve_or_build` / `build_with`: the T1w (and the T2w beside it, equal weights) is mirrored with `flip_x`, written to a work folder, and registered template (fixed) ← mirror (moving) by `norm_native::register_templates`, i.e. the engine and recipe of the template bridge (`SynRecipe::cohort`, multispectral with two contrasts; rigid + affine + SyN on the template's own grid). The engine is the in-process one chosen by the normalize-step `--reg-backend` (`choose_for_run`); there is no ANTs fallback and no slow CPU fallback: without a usable GPU adapter the build fails with `E_GROUP_FLIP`, and the way out is `--flip-transform FILE` or `--plain-flip`. The composites are renamed to `from-<space>_to-<space>Flipped_mode-image_xfm.h5` (forward: warps the mirrored image onto the template grid) and `from-<space>Flipped_to-<space>_mode-image_xfm.h5` (inverse: maps points of the mirrored space to the template) and stored in `<user data dir>/templates/space/<space>/fliplr/` (`DBSLOC_DATA_DIR`; one folder per space, not per resolution) with `build.json` (software version, space, engine description, role / file name / SHA-256 of every template, timings and the registration record). The work happens in a sibling `.fliplr.partial-<pid>` folder that is renamed on success, so a failed or cancelled build keeps the previous transform. Lookup order: user data folder (accepted without `build.json`, i.e. placed by hand; with `build.json` only while its template fingerprint equals the current template files, else it is stale and ignored), then a `fliplr/` folder beside the template (`<template folder>/fliplr`, `<space folder>/fliplr`, `<install>/resources/templates/space/<space>/fliplr`), with the explicit `--flip-transform` forward composite overriding both. *Calculate* with `mirror` resolves the transform once per template space of the subject's `stimulations/<space>/` folders (a failure is remembered for the rest of the run), builds a missing one first (log line `building the left-right flip transform of <space> (minutes)...`, cancellable through the batch cancel flag, a cancel marks the subject cancelled) and fails the subject with `E_GROUP_FLIP` and the §13 hint when the build fails; `--plain-flip` (`FlipTransformChoice::PlainFlip`) writes the x flip only and reports a warning per subject. Images: `flip::flip_nifti` flips about x (`flip_x`), then pulls every voxel centre of the mirrored grid (the source grid with x negated) through the forward composite (`fastwarp_reg::apply_chain`; nearest neighbour for binary, linear for E-fields); the output is written on that mirrored grid with the Lead-DBS names and codes (corrected 2026-11-12: until then the samples were taken on, and written with, the original grid, which is right only for a grid symmetric about x = 0 and gave empty files otherwise). The readers (`sweetspot`, `fibrefilter`, `view`) use a flipped file only when `flip::flipped_is_current` finds it on the mirrored grid of its original (header only: same dimensions, affine equal to the mirrored one up to single precision), and otherwise treat it as missing and ask for *Calculate* with mirroring again. Points: `flip::flip_points_through` negates x and maps through the inverse composite found by file name (`RustPointWarp`, the lattice route of `specs/05` §6). `dbsloc group fliplr build [--template PATH|ID] [--force]` and `dbsloc group fliplr show` manage the cache (`docs/RUN_REFERENCE.md`). **Not implemented yet; planned (owner decision 2026-11-13), convention to be specified:** the Lead-DBS `fliplr/Composite.nii.gz` import as a cache entry. `fastwarp_reg::transform::read_transform_file` does read a `.nii` / `.nii.gz` ANTs displacement field, so such a file given to `--flip-transform` is applied to images as a one-element chain, but the spec does not fix its direction, its coordinate convention (ITK LPS vs RAS) or whether it contains an affine, and a NIfTI field has no inverse composite, so `flip_points_through` refuses it; nothing is converted or copied (**UNSPECIFIED**: §4.4 needs the file's convention before an import can be verified). The §5.4 mirrored patient copies (`<name>_mirrored`: markers swapped and mapped through `flip_points_through`, contact values mirrored) are written by the group 3D view (`group/view.rs`). Tests: `group::fliplr::tests` (phantom build, about 13 s on the CPU reference engine with its shortened schedule: Dice ≥ 0.9 double flip of a sphere, centroid |x + x'| ≤ 2 mm, markers through the inverse, E-field maximum kept, Lead-DBS names and the mirrored grid on a symmetric and an off-centre grid, off-centre binary and E-field flips, lookup order and stale `build.json`, inverse names), `group::calc::tests` (build once for two subjects, failed build hint, plain flip warning), `cli_group_calc` (flag exclusion, `fliplr show`). **Real template (2026-10-23, Intel Arc 140V, wgpu):** `dbsloc group fliplr build` on the shipped MNI152NLin2009bAsym template took 19.6 s of registration (linear 4.7 s, SyN 14.5 s), 22.6 s wall, and stores two composites of 208 MB each (affine part near identity, x translation −0.098 mm). The ignored test `real_template_flip_round_trip` on the 1 mm grid: double flip of a 4 mm radius sphere at (+12, −14, −6) mm Dice 0.9981 (§11 test 8: ≥ 0.9); markers (12, −14, −6) → (−12.12, −14.76, −5.97), (−11, −13, −8) → (10.87, −12.33, −7.97), (12.5, −10, 3) → (−12.42, −11.21, 3.18), all within 2 mm of the mirrored position; displacement against the plain x flip over a 5 mm grid in a 60 mm box: maximum 3.63 mm, 99th percentile 2.67 mm. Other adapters (NVIDIA, Metal) are unverified.
- **§5 group 3D view (implemented 2026-10-23, `group/view.rs`, `crates/dbsloc-viz/src/group_scene.rs`).** `dbsloc group view <group> [--colour-by group|<variable>] [--normalisation none|zscore|van_albada] [--mirror]` writes `viz/scene_manifest.json`. `colour_by.normalisation` values are `none`, `zscore`, `van_albada`; each subject entry has `value` per side plus `value_kind` (`contact` or `pair`, midpoints), `mirrored` and `source_subject`; a mirrored copy's reconstruction is `viz/mirrored/<name>_mirrored_desc-reconstruction.json` and its `vta` are the flipped files. The flip transform is looked up, never built, by the view (plain flip with a warning otherwise); a patient in another space uses its `reference` block when the group space is `MNI152NLin2009bAsym`, else is left out (`W_GROUP_SPACE`). `isomatrix` shape order: width 1, 2, 2(Kmax-1), 2Kmax. Contact markers (spheres) are drawn for highlight-active, a changed show-active / show-passive filter, a lead without a catalogue mesh and the point cloud; the bodies are always drawn. VTA surfaces: yellow α 0.35, or the group colour when colouring by group; leads outside a selection are drawn at α 0.3. Not implemented: §5.6 isovolume and statvat maps, selecting a lead by click; the §5.5 performance targets are unmeasured.
- **§6.3 / §6.4.** `n` in a summary counts patients with a volume above 0 on that side. The Pearson / Spearman permutation correlations and the two-group test are in `group/atlas_stats.rs` (`dbsloc group correlate`): points are patients (per-patient variable; `both` = right + left with a missing side 0, `right` / `left` only patients with a VTA on that side) or sides (per-hemisphere variable; `both` stacks right then left, a side without a VTA or a value is dropped); all n! permutations are enumerated up to 7 points (p = hits / n!), above that N seeded random permutations give p = (hits + 1) / (N + 1) (default 5 000, seed 1), both statistics share the permutations; fewer than 3 points gives empty values and fewer than 3 patients with a VTA gives `E_GROUP_TOO_FEW`. The two-group test is the pooled-variance Student t-test (df = n1 + n2 − 2) for a per-patient variable with exactly two finite values, group 1 = lower value; quartiles use linear interpolation, whiskers 1.5 IQR, outliers listed, in `atlasstats/clinical-<variable>-<measure>/analysis.json`. `fibercounts` / `nfibercounts` are written to the export only when a patient's `ea_stats` holds `ft` for the label. **UNSPECIFIED in §6.3:** the treatment of a side without a VTA for a per-hemisphere variable (dropped here), the quartile definition (linear interpolation here), whether the Monte-Carlo p counts the observed labelling (it does here) and the default seed.
- **§7.** Patients are the included ones (or `--subjects`), not `view.list_select`. Cross-validation and the saved model use the pooled map; `--split-by-group` adds per-group map files. Permutations use one RNG stream per permutation (thread-count independent). Rows without a covered voxel predict NaN. AUC is the logistic AUC over pooled predictions. Bend correlation is deferred. Wilcoxon signed-rank is exact up to 15 untied differences. Under mirroring the original row only is a test sample. §7.8 test 1 as written leaves almost no voxels at c = 20 % with 40 spheres in a 12 mm cube: the test fixes seed 21, S* at the cube centre, c = 10 % for the max-t check and c = 5 % for LOO / LNO; the spec should state a coverage threshold or cohort density.
- **§8.** Sigmoid applies to connected values only; prediction means run over touched shown fibres; peak / peak-5 % are sign-aware; AUC = Mann-Whitney of the predictions (flipped when r < 0); the weighted two-sample form stacks each patient twice (df = 2n − 2), the weighted one-sample form is not implemented; a missing E-field fails with `E_SWEETSPOT_NO_EFIELD` in E-field modes; the stored model keeps the shown fibres only; `fibrescores_<R|L>.tsv` (every scored fibre) is an extra output; the optional `.trk` / density exports are not written. Fewer than 3 patients with an outcome → `E_GROUP_TOO_FEW`.
- **§9.** Masks: `grey` = `gm_mask.nii.gz` of the record's atlas set (else the first set that has one), `brain` = the template brain mask, `all`, or a file; cortex / cerebellum masks have no files and are not offered. Seed maps are found by the `specs/15` §9.2 name patterns (flipped: `hemidesc-Flipped…`); all maps must share one grid (`E_CONN_SPACE`). The map file is `dataset-<d>_analysis-<g>_netmap-<g>_stat-rmap_map.nii.gz`, unfiltered by sign; similarity uses Fisher z only with `--fisher-z`; per-fold maps and the two-sample t map's group input are not implemented. New-patient prediction (§8.5 / §9.4) exists as library functions only.
- **§10.3.** A table with no matching subject is a plain `GroupError::Clinical`; unknown subjects are skipped and reported.
- **Statistics.** All group statistics (tests, correlations including bend, corrections, regression, `Rng`) live in `group/stats_common.rs`; fibre filtering and network mapping share the sweet-spot explorer's Wilcoxon tests (continuity correction, exact signed-rank up to 15 untied differences) and Yates proportion test.

## 15. Owner decisions (2026-10-23)

Decisions of the product owner that scoped `specs/07`, this spec and `specs/15` (moved here from the retired Lead Group feasibility study, which remains in git history).

| # | Decision |
| - | -------- |
| 1 | Both use cases from the start: research group analyses and single-patient programming support. |
| 2 | E-field VTAs are required in the first release; the analytical models remain as clearly labelled extra options. |
| 3 | Full Lead Group migration: read existing Lead Group project files and per-subject `ea_stats.mat`; write files Lead Group can read back where feasible. |
| 4 | No OSS-DBS / conda dependency: everything in-process (Rust, wgpu where natural). |
| 5 | (Superseded for GSP 1000 v2, dTOR-985 and WU-Minn HCP by `specs/15` D10 of 2026-11-10.) Downloading from lead-dbs.org is allowed as long as the site is not abused: one download at a time, resume, no retry storms, product User-Agent. Connectomes that need the user's own data-use acceptance (HCP / GSP / PPMI) are listed in the GUI with a link to the dataset site and step-by-step instructions, then imported from a folder. Both structural and functional connectomes. |
| 6 | Statistics scope: atlas overlap tables, sweet-spot mapping, fibre filtering and network mapping (R-map). |
| 7 | Desktop and the GPU server from the start (`docs/SERVER_CLIENT_PLAN.md` gains a `vat` stage and cohort jobs). |
| 8 | VTAs computed in native space, warped to template space, both stored; group analyses read the template-space files. |
| 9 | Full multi-source (independent current sources), bipolar / multipolar and case contact in the data model; voltage and current mode. |
| 10 | Maximal Lead-DBS compatibility: clinical variables stored the way Lead Group does (inside the group file) plus a TSV export. |
