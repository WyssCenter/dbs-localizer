# DISTAL atlas (Ewert et al. 2017) and other Lead-DBS atlas folders — Wyss DBS Localizer distribution policy

**Reference (cite in publications):**  
Ewert, S., Plettig, P., Li, N., Chakravarty, M. M., Collins, D. L., Herrington, T. M., et al. (2017). Toward defining deep brain stimulation targets in MNI space: A subcortical atlas based on multimodal MRI, histology and structural connectivity. *NeuroImage*.

**Space:** ICBM 2009b NLIN Asym (MNI152 family), consistent with Wyss DBS Localizer template normalization when using that template.

## Decision of 2026-10-22: bundle the Lead-DBS atlas folders, GPL licence beside them

The product owner decided to ship the Lead-DBS atlas **data** folders in-tree (the product licence became GPL-3.0-only on 2026-11-10, and the user wants the same default atlas and colours as Lead-DBS). Rules:

1. Folders are copied **verbatim** from `templates/space/MNI152NLin2009bAsym/atlases/<name>/` of the Lead-DBS data package `leaddbs_data.zip` (downloaded separately from the Lead-DBS host; the Lead-DBS git repository ignores `atlases/`) into `resources/atlases/leaddbs/<name>/` (masks under `lh/`, `rh/`, `mixed/`, `midline/`, the `atlas_index.mat` cache with names / colours / presets / citation, `gm_mask.nii.gz`). No Lead-DBS code.
2. Lead-DBS's `LICENSE.md` (GPL-3; it states that the atlases "included in the repository" are released under that licence at their owners' request and should not be used commercially) is kept beside them as `resources/atlases/leaddbs/LICENSE.md`, and every bundled atlas is listed in `NOTICE` with its citation and the Lead-DBS version its data package was installed with. The data package itself holds no licence file, and the Lead-DBS maintainers stated on 2026-11-11 that the atlas packages carry no licence. Owner decision of 2026-11-11: keep the GPL label and the request not to use the atlases commercially, as Lead-DBS states them.
3. Bundled today: **DISTAL Minimal (Ewert 2017)** (28 MB, the Lead-DBS default atlas, `specs/11` §1.3). Larger sets (DISTAL Medium 66 MB, DISTAL 808 MB) are not committed; download them with `dbsloc atlas fetch <name>` (`specs/11` §10) or use an external Lead-DBS atlases directory (below).
4. The atlas owners ask that the atlases are not used commercially; this request travels with the data (`resources/atlases/leaddbs/LICENSE.md`). Before any commercial use or distribution: obtain the atlas owners' terms for the files actually shipped, or drop the folders from the archive and keep only the download and external-directory paths.

## Loading atlases (3D viewer)

The viewer lists every atlas set the pipeline writes into the scene manifest (`specs/11`, `crates/dbsloc-pipeline/src/atlas_catalog.rs`):

- the bundled Lead-DBS-layout folders under `resources/atlases/leaddbs/*` (default: `DISTAL Minimal (Ewert 2017)`, colours, opacity and the initially shown structures from its `atlas_index.mat`),
- every folder of an external Lead-DBS atlases directory (`DBSLOC_LEADDBS_ATLASES_DIR=<Lead-DBS>/templates/space/MNI152NLin2009bAsym/atlases`, or the GUI setting), so a user with a Lead-DBS installation sees all its atlases without copying them,
- the AHEAD masks (`docs/ATLAS_AHEAD.md`, CC BY 4.0, flat layout),
- and the manifest overrides `DBSLOC_SCENE_ATLAS_LEAD_DIR` / `DBSLOC_SCENE_ATLAS_OBJ_DIR` / `DBSLOC_SCENE_ATLAS_NII` (a directory of `.obj` files, one mesh per structure; or a single labelled NIfTI meshed with marching cubes).

See the manifest schema in `crates/dbsloc-viz/src/scene_manifest.rs`.
