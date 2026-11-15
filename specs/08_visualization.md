# Spec 08 — Visualization: scene outputs and viewers

**Status:** The scene manifest writer (`reconstruct`, `visualize`), per-set atlas warping, the 3D scene viewer with its layers and group scene, the *Refine localization* view and the GUI locks are implemented; 2D slice images and a methods text (soon) and a `scene.glb` export (later) are planned.
**Depends on:** `00` (invalidation §6.1), `01` (derivative names §7a, image import §8.4), `04` (templates §3, template space bridge §5.4a), `05` (Reco v2 §8, manual correction §9a), `06` (roll convention §10.6), `07` (VTA images), `09` (electrode catalogue, contact names §2.3, lead bodies §11), `11` (atlas folders, meshing, colours, licences), `12` Part 2 (reference correction view), `14` (group scene §5, explorers §11a), `15` (fibre files). The registration review viewer is specified in `03` §12 and [`08_addendum_interactive_volume_viewer.md`](08_addendum_interactive_volume_viewer.md).

---

## 1. Scope and conventions

This spec covers what the program writes for visualization, the 3D scene viewer, the *Refine localization* view and the GUI rules around them (§11; the numbers §11.1.5–§11.1.9 are cited elsewhere and stay fixed). Not covered: the registration review (`03` §12 and the addendum), atlas folder formats and meshing rules (`11`), group analyses (`14`), VTA computation (`07`), connectome computation (`15`).

Conventions: world coordinates are RAS millimetres of the scene's template space, the space of `template_nii` (§7.2). Side key `"1"` is the right lead, `"2"` the left (`05` §8.3). Paths in a manifest written by the pipeline are absolute.

---

## 2. The `visualize` stage and the scene manifest writer

### 2.1 Stage

`visualize` is the last stage of the default sequence (`crates/dbsloc-protocol/src/stage.rs`); the GUI calls the step *Write electrode image* (`crates/dbsloc-ui/src/settings.rs`). It writes the subject's scene manifest and nothing else (`crates/dbsloc-pipeline/src/stage_impl.rs`, `fn run_visualize`). `reconstruct` writes the same manifest after every successful reconstruction (`fn write_reconstruction`, `05` §9), so a fresh reconstruction always has a scene.

| Item | Rule |
|---|---|
| Prerequisites (`prereqs.rs`) | `normalization/anat/` exists and holds a normalized image, and the reconstruction file exists (messages in §8). |
| No template | `RunOptions.normalization_template` unset or not a file: logs `[visualize] no template set — skipping scene manifest`, outcome skipped `visualize: no normalization template set`. |
| Template or reconstruction gone at write time | skipped: `visualize: scene manifest not written (template or reco missing)`. |
| Success | logs `[visualize] scene manifest: <path>` (`[reconstruct] scene manifest: <path>` from `reconstruct`), token `ok:visualize:scene_manifest.json`. |
| Lock inputs (`locks.rs`) | the first normalized post-op image (else the first normalized pre-op image) and the reconstruction file. |
| Invalidation | invalidated when an upstream stage re-runs (direct parents: `reconstruct`, `orientation`, `vat`) and by a manual correction (`05` §9a); invalidates nothing (`00` §6.1). |

### 2.2 Manifest writer

`stage_impl.rs`, `fn write_scene_manifest` (collects the atlas sets and the bridge) and `fn write_scene_manifest_with`:

1. Nothing is written when the template or the reconstruction file is missing.
2. Path: `<output dir>/sub-<id>/reconstruction/viz/scene_manifest.json` (`derivatives.rs`, `fn scene_manifest`); the folder is created.
3. `space` is the template's space id (`template_catalog.rs`, `fn infer_space`; `04` §3.1).
4. Fields written: `manifest_version` = 1, `subj_id`, `space`, `template_nii`, `reco_json`, `backdrops` (§11.1.7), `vta` when the subject has binary VTA images in that space, `atlas_obj_dir` / `atlas_nii` when `DBSLOC_SCENE_ATLAS_OBJ_DIR` / `DBSLOC_SCENE_ATLAS_NII` are set, and `atlas_lead_sets` plus `atlas_default_id` when at least one atlas set is placed (§5).
5. The file is written to a temporary name and renamed (`outputs::write_replacing`).

**VTA entries** (`fn scene_vta_entries`): among the label folders of `stimulations/<space>/` (folders starting with `.` skipped), the label whose binary VTA images (`*_sim-binary_model-*_hemi-R.nii`, `*_hemi-L.nii`, `07` §5.1) were modified last; one entry `{side: "right" | "left", label, path}` per image found.

A manifest error inside `reconstruct` only logs `[reconstruct] scene manifest write failed: <error>`; the reconstruction stands. This includes a cancel while the template space bridge is built. In `visualize` a manifest error fails the stage, and a cancel during the bridge build cancels it.

### 2.3 Viewer launch after `reconstruct`

Only `dbsloc run` and `dbsloc_pipeline_runner` launch a viewer. When `reconstruct` has written the manifest and neither `RunOptions.headless` (`--headless`, `DBSLOC_HEADLESS=1`) nor `RunOptions.no_scene3d` (`--no-scene3d`, `DBSLOC_OPEN_SCENE3D=0`) is set, `fn launch_scene3d` logs `[reconstruct] launching <program>`, starts `dbs-localizer scene --manifest <absolute path>` and waits until the window is closed; cancelling the run closes it. Without a display (`display.rs`, `fn has_display`) or without the program it logs a note instead (§8) and the run continues. The program is found as `DBSLOC_GUI_BIN` → beside the running executable → `PATH` → `target/{release,debug}` of a source tree (`paths.rs`, `fn gui_binary`).

The `visualize` stage never launches a viewer. The GUI and the server run every stage with `headless` and `no_scene3d`; the GUI shows the scene in its own *3D Viewer* tab (§4.2).

---

## 3. Planned outputs (owner decision 2026-11-13)

Nothing writes these files today. Each entry states what is decided; lines marked *Proposal* are open until the owner confirms them.

### 3.1 2D slice images — **Planned** (soon)

- **Content.** PNG images of axial, coronal and sagittal slices of the subject's template-space anatomy with the reconstructed leads and contacts drawn on them.
- **Location.** The subject's output folder. *Proposal:* `reconstruction/viz/slices/`; one PNG per contact level and plane, its file name carrying the compact contact or level name of `09` §2.3 (for example `K0R`).
- **Trigger.** *Proposal:* written by the `visualize` stage after the manifest; a new run replaces the folder's content.
- *Proposal:* slices through each contact level, cropped so that every contact of the side is visible; contact outlines from the scene block's `coords_mm`, the lead axis, and the default atlas set's initially visible structures as coloured contours; annotated with subject, side, plane, the plane's world coordinate, orientation letters and a millimetre scale bar.

### 3.2 Methods text — **Planned** (soon)

- **Content.** Plain UTF-8 text for the Methods section of a paper, built only from recorded provenance, with the references of the methods named.
- **Location.** `<output dir>/sub-<id>/log/sub-<id>_desc-methods.txt`, the file name Lead-DBS uses (`12` §6, `13` §12).
- **Trigger.** Every stage run appends one entry; the file is never rewritten, so a Lead-DBS log imported with the subject keeps its history.
- *Proposal:* an entry holds a timestamp, the stage name, method sentences from the stage's method log and the reconstruction `meta` (software version `05` §8.7, engine and settings, template and space, electrode model, orientation and manual corrections, VTA model) and the references; the `visualize` entry names the atlas set shown with its citation.

### 3.3 Scene export `scene.glb` — **Planned** (later)

- **Content.** A binary glTF 2.0 file `scene.glb` with the geometry of the 3D scene: lead bodies, visible atlas structures with their colours and opacities, VTA surfaces and map isosurfaces.
- **Location.** *Proposal:* `<output dir>/sub-<id>/reconstruction/viz/scene.glb`, or a path the user chooses (default `<home>/<subject>_scene.glb`).
- **Trigger.** *Proposal:* on demand from the viewer (*Save scene* offers `.glb`); not written by the pipeline.
- *Proposal:* positions converted to glTF's y-up frame (x_gltf = x, y_gltf = z, z_gltf = −y); node names carry subject, side and structure; slices, fibres and labels are not exported.

---

## 4. The 3D scene viewer

### 4.1 Program and command line

The viewer is a mode of the GUI program (`crates/dbsloc-app/src/main.rs`: first argument `scene`; `crates/dbsloc-viz/src/scene_cli.rs`, `fn run`):

```
dbs-localizer scene --manifest <path> [--headless-check] [--verbose]
                    [--navigation lead-dbs|classic] [--backdrop <id>] [--atlas <id>]
```

- The window opens maximised with the title `Electrode reconstruction scene — Subject <id>`.
- `--navigation` chooses the mouse scheme (§11.1.5; default `lead-dbs`). `--backdrop` starts on one of the manifest's backdrops (§11.1.7). `--atlas` starts on one of its atlas sets (§5.4). `--verbose` sets `DBSLOC_VIZ_VERBOSE=1` (diagnostics on stderr).
- `--headless-check` loads the manifest, template and reconstruction, meshes the initially visible structures of the default set, prints `scene manifest v<n>: subj=<id> space=<space> atlas_groups=<n> atlas_structures=<n> atlas_lead=<n> atlas_objs=<bool> atlas_nii=<bool>` and `active_atlas=<id> structures_shown=<n> structures_meshed=<n>` (plus `group <guid>: leads=<n> patients=<n> models=<n> vta_surfaces_to_mesh=<n>` for a group scene) and exits without opening a window. It does not apply `--atlas` (`active_atlas` reports the default set) and does not check `--backdrop`.
- Exit codes: 0 on success; 1 when the manifest or the scene cannot be loaded (`Error: <details>`); 2 for an unknown navigation scheme or atlas id, and for an unknown backdrop id when the window opens (§8).
- `DBSLOC_SCENE3D_AUTOSAVE_PNG=<file>` saves the first frame drawn after the atlas and the backdrop have loaded.

`dbsloc viz scene --manifest <path> [--headless-check] [--navigation lead-dbs|classic]` (`crates/dbsloc-cli/src/cmd/viz.rs`) starts that program and returns its exit code; 127 when the program is not found. It does not forward `--backdrop` or `--atlas`.

### 4.2 In the GUI

The *3D Viewer* tab embeds the same viewer (`crates/dbsloc-app/src/panels.rs`, `ScenePanel`):

- It loads the selected subject's `reconstruction/viz/scene_manifest.json` as soon as that file exists. *Scene source* holds the manifest path (*…* to browse, *Load scene*, and *Use default path* after a manual choice). A manifest chosen by hand stays until the subject, the dataset or the output folder changes, or until the file is gone at start-up.
- Loading runs on a worker thread (*Reading the scene manifest and the template volume…*); a failure shows *The scene could not be loaded* with the details.
- *Open the 3D viewer when reconstruction finishes* (Reconstruct card, on by default) focuses the tab and reloads the subject's scene after `reconstruct`. The guided *Open 3D viewer* loads the scene, running `visualize` first when no manifest exists. *Show in 3D viewer* of the *Group analysis* tab loads the group manifest (`14` §5). After a saved manual correction, a loaded scene of that subject reloads and keeps the view (`fn reload_loaded_under`).
- *Open Viewer in separate window* (F11) moves the viewer to its own window; *Dock to main window* brings it back.
- The view state (`SceneViewState`: camera, projection, slices, backdrop, atlas set and structure settings, toggles, fibre width and maximum) is saved with the GUI settings (`SceneSettings.view`) and restored on the next load. The navigation scheme is saved as `DbslocAppState.navigation`.
- The application can push VTA surfaces (stimulation editor *Show VTA*) and fibre and map layers (§11.1.8) into the loaded scene.

### 4.3 What the scene shows

| Layer | Source | Contract |
|---|---|---|
| Anatomy slices | the chosen backdrop | §11.1.7 |
| Lead bodies | Reco markers and the catalogue model | §11.1.2 |
| Contact markers and labels | Reco `coords_mm` | §11.1.2 |
| Trajectory polyline | Reco `trajectory` | only for a side without a lead body, §11.1.2 |
| Atlas structures | the active atlas set | §5 |
| VTA surfaces | manifest `vta` (binary VTA images, `07` §5), or pushed by the stimulation editor | marching cubes of the binary image (`vta_mesh.rs`) |
| Fibre and map layers | pushed by the application | §11.1.8 |
| Group leads, contacts and VTAs | manifest version 2 | §11.1.4, `14` §5 |
| Orientation letters R, L, A, P, S, I and a millimetre scale bar | camera | screen overlay |

**Reading the reconstruction** (`reco_json.rs`, `fn load_block_side_geometry_from_str_opt`). The viewer reads the `mni` block (the run's template space) of a Reco v2 file (`05` §8), sides `"1"` and `"2"`. One side is enough; a file with neither fails with `<path>: the reconstruction has neither a left nor a right lead`. A side without `trajectory` uses `head` and `tail` as a two-point trajectory. Tail markers of files written before 2026-11-14 are converted on read (`05` §8.8) and the conversion warning is printed. The electrode model of a side is `props[<k>].elmodel`, else `meta.elmodel` (`05` §8.4).
Implementation status: the viewer reads `meta.elmodel` only, so a file whose sides carry different models draws both sides with the `meta` model.

---

## 5. Atlas sets

### 5.1 Sets listed in a manifest, and the default

The pipeline lists the sets in this order (`crates/dbsloc-pipeline/src/atlas_catalog.rs`, `fn atlas_sets`; `11` §1, §10.5):

| # | Source | id | Notes |
|---|---|---|---|
| 1 | `DBSLOC_SCENE_ATLAS_LEAD_DIR`, when it names a folder | `override` | name = folder name, Lead layout, its `atlas_index.mat` when readable; without an index the AHEAD name list below sets the initial visibility. Used as given: never warped, the user vouches for its space. Not the default, unless its folder is named `DISTAL Minimal (Ewert 2017)`: the default rule matches by name and this set comes first. |
| 2 | bundled `resources/atlases/leaddbs/<folder>` | `leaddbs:<folder>` | |
| 3 | downloaded `<user data dir>/atlases/leaddbs/<folder>` (`11` §10) | `user:<folder>` | skipped when this folder is the external directory |
| 4 | the Lead-DBS atlases directory: `RunOptions.leaddbs_atlases_dir` (`--leaddbs-atlases-dir`, `DBSLOC_LEADDBS_ATLASES_DIR`, GUI *Lead-DBS atlases directory* on the *Write electrode image* card) | `external:<folder>` | read in place, never copied |
| 5 | bundled AHEAD masks `resources/atlases/ahead/structures_mni09b/` | `ahead` | name `AHEAD basal ganglia (Alkemade 2020)`, flat layout; structures whose name contains `STN`, `GPi`, `GPe`, `RN`, `SN`, `subthalamic`, `pallidus` or `red` (case-insensitive) start visible |

Folders whose name starts with `.` and sets whose folder is missing are left out. A Lead-layout set carries its parsed `atlas_index.mat` (`11` §3) as `index`; an index that cannot be read is logged and the set is kept without it. A subject's list changes on its next `reconstruct` or `visualize` run. A manifest may also name `atlas_obj_dir` (a folder of `.obj` meshes, one structure per file; set id `obj-directory`, label *OBJ directory*) and `atlas_nii` (an integer label volume meshed per label; set id `labeled-nifti`, label *Labeled NIfTI*); these follow the Lead-layout sets. Licences and citations of the atlases: `11` §6, `docs/ATLAS_DISTAL.md`, `docs/ATLAS_AHEAD.md`.

**Default set** (`fn default_atlas`; `11` §1.3): the first Lead-layout set in the order above named `DISTAL Minimal (Ewert 2017)` (so the bundled copy comes before an external one), else `ahead`. Its id is written as `atlas_default_id`. A manifest without `atlas_default_id` (for example one written by hand) shows the set labelled `DISTAL Minimal (Ewert 2017)`, else the first set (`crates/dbsloc-viz/src/atlas_sets.rs`, `fn default_set_index`).

### 5.2 Atlas in a non-reference template space

Every set is in `MNI152NLin2009bAsym` (`11` §1). When the scene's space is that space, and for the `override` set, the set's folder is used directly. Otherwise each set is warped on its own (`stage_impl.rs`, `fn warp_atlas_into_template`):

1. The template space bridge of `04` §5.4a is obtained (built once and cached).
2. Every mask NIfTI of the set (root and `lh/`, `rh/`, `mixed/`, `midline/`) is resampled onto the template grid with linear interpolation into `<bridge dir>/atlases/<folder>/structures/`, keeping relative paths. `<folder>` is the set id with every character other than an ASCII letter or digit replaced by `_`, cut to 64 characters, then `-` and the first 12 hexadecimal digits of the SHA-256 of the id (`fn atlas_cache_folder`). The run logs `dbsloc: note: scene: warping <n> atlas masks of <id> into <space>`.
3. The output is staged in `<bridge dir>/atlases/<folder>/.partial-structures` and renamed into place. A folder in which every mask is already present is reused without warping.
4. The manifest entry's `dir` points at the warped folder; `index` is the original set's.

A set that cannot be warped is left out with `dbsloc: warning: scene: no atlas <name> for template space <space>: <reason>`, where `<reason>` is `the template space bridge is unavailable` (for example `DBSLOC_NO_BRIDGE=1`) or the bridge or warp error. When no set remains, the manifest has neither `atlas_lead_sets` nor `atlas_default_id`.

### 5.3 Meshing and the mesh cache

Each structure side is meshed on the mask's own voxel grid with the threshold and smoothing of `11` §4.2 and §9 (`crates/dbsloc-viz/src/atlas_marching.rs`). When the scene opens, only the initially visible structures of the default set are meshed; other structures and sets are meshed in the background the first time they are shown (*Preparing atlas surfaces* with progress).

`<cache>` is `<base>/dbsloc/cache`, with `<base>` = `%LOCALAPPDATA%` on Windows, `~/Library/Caches` on macOS, `$XDG_CACHE_HOME` (else `~/.cache`) elsewhere (`fn user_mesh_cache_dir`). Atlas meshes are looked up first in `precomputed_obj/` beside the masks (`<structure>.obj` + `<structure>.cache.json`, read only), then in `<cache>/atlas_meshes/` (`<structure>-<16 hex digits>.obj` + `.cache.json`). The key (version 6) holds the mask's SHA-256, the meshing parameters of the manifest, the threshold and the hemisphere split. New meshes go to `<cache>/atlas_meshes/` only, each file written to a temporary name and renamed. Group-scene VTA meshes are cached beside it in `<cache>/vta_meshes/` (`group_scene.rs`; the system temporary folder when no cache folder can be determined).

### 5.4 Choosing the set in the viewer

The *Atlas* section's *Atlas set* combo lists the sets (label = set name, hover = short citation; the full citation is shown under the combo). The chosen id is `SceneViewState.atlas_id`, set from the saved view by the GUI or by `--atlas <id>`. Rule (`scene3d_gpu.rs`, `fn resolve_active_atlas`, `fn atlas_pick_records`):

- When the loaded scene lists the chosen id, that set is shown.
- Otherwise the default set (§5.1) is shown and the chosen id is kept unchanged, so a scene without that set never overwrites the user's choice; the next scene that lists it shows it again.
- Picking an entry in the combo, including the default set already shown, records its id.
- `--atlas <id>` with an id the manifest does not list exits 2 (§4.1).

Structure visibility, colours, presets and label sets follow `11` §5; the controls are listed in §11.1.3.

---

## 6. Colours and materials

RGBA values in 0–1.

| Element | Colour | Source |
|---|---|---|
| Lead insulation (shaft, tip that is not a contact) | (0.20, 0.21, 0.24, 1) | `electrode_mesh.rs`, `INSULATION_RGBA` |
| Contacts, and a tip that is a contact | (0.80, 0.82, 0.86, 1) | `METAL_RGBA` |
| Contact markers when labels are on | #56B4E9 disc with an outline | §11.1.2 |
| VTA surface, single-subject scene | (0.93, 0.27, 0.22, 0.6) | `vta_mesh.rs`, `VTA_RGBA` |
| VTA surface, group scene | the group colour when colouring by group, else (0.95, 0.80, 0.15); α 0.35, × 0.3 for dimmed leads | `group_scene.rs`, `14` §5.2 |
| Active contacts, group scene | (0.9, 0.1, 0.1) with a (0.9, 0.9, 0.7) edge; dimmed leads α 0.3 | `14` §5.2 |
| Atlas structure | colour and α from `atlas_index.mat`, else the palette of `11` §5.1 and α 0.7 (`11` §4.4); flat sets and `.obj` meshes get a colour derived from the name | `atlas_sets.rs`, `atlas_marching.rs` |
| Map isosurfaces | positive (0.9176, 0.2, 0.1373), negative (0.2824, 0.6157, 0.9725), α 0.5 | §11.1.8, `14` §7.7 |
| Fibres | per layer colouring | §11.1.8 |

---

## 7. Output files and the scene manifest

### 7.1 Files

| File | Written by | Status |
|---|---|---|
| `<output dir>/sub-<id>/reconstruction/viz/scene_manifest.json` | `reconstruct`, `visualize` | implemented, §2 |
| `<group folder>/viz/scene_manifest.json` (version 2) | `dbsloc group view`, GUI *Group analysis* | implemented, `14` §5.1 |
| `<bridge dir>/atlases/<folder>/structures/` | manifest writer | implemented, §5.2 |
| `<cache>/atlas_meshes/` | viewer | implemented, §5.3 |
| `<cache>/vta_meshes/` | group scene | implemented, §5.3, `14` §5.5 |
| `<data folder>/backdrops/space/<space>/<id>/` | `dbsloc backdrop fetch`, GUI | implemented, §11.1.7 |
| scene image (`.png`, `.jpg`) at a path the user chooses | viewer *Save scene*, `DBSLOC_SCENE3D_AUTOSAVE_PNG` | implemented, §11.1.3 |
| 2D slice PNGs in the subject's output folder | — | **Planned**, §3.1 |
| `<output dir>/sub-<id>/log/sub-<id>_desc-methods.txt` | every stage run (appended) | **Planned**, §3.2 |
| `scene.glb` | — | **Planned**, §3.3 |

### 7.2 Manifest version 1

A JSON object (`crates/dbsloc-viz/src/scene_manifest.rs`, `SceneManifestV1`); unknown fields are ignored.

Fields written by the pipeline:

| Field | Type | Meaning |
|---|---|---|
| `manifest_version` | integer | 1; 2 marks a group scene (§7.3); default 1 |
| `subj_id` | string | subject id without `sub-` |
| `space` | string | template space id; also the key of the backdrop catalogue |
| `template_nii` | path | the scene's reference image, in the same space as the Reco `mni` block. It sets the scene bounds, the default camera target and the `template-t1` backdrop, and it is the grid for meshing when resampling is switched on |
| `reco_json` | path | Reco v2 file (`05` §8); empty in a group scene |
| `backdrops` | array of `{id, label, path, kind}` | slice images, §11.1.7 |
| `vta` | array of `{side, label, path}` | binary VTA images, §2.2 |
| `atlas_lead_sets` | array of `{name, dir, id, layout, default_on_substrings, index}` | atlas sets, §5.1. `id` defaults to `name`. `layout` is `lead` or `flat`; when absent it is `lead` if any of `lh/`, `rh/`, `mixed/`, `midline/` exists. `default_on_substrings` lists the names that start visible (sets without an index). `index` is `atlas_index.mat` in the form of `11` §9 |
| `atlas_default_id` | string | id of the default set, §5.1 |
| `atlas_obj_dir`, `atlas_nii` | path | extra atlas groups, §5.1 |

The reader also accepts fields the pipeline never writes (a single legacy atlas folder, the label limit of `atlas_nii`, meshing and cache parameters, debug switches); their names and defaults are those of `SceneManifestV1`.

**Reading** (`SceneManifestV1::from_json_file`). Relative paths resolve against the manifest's folder. `template_nii`, a non-empty `reco_json`, `atlas_obj_dir`, `atlas_nii`, `atlas_lead_nii_dir` and every set `dir` must exist; backdrop, VTA and group paths are made absolute without that check. Integer fields of an `index` accept integer-valued numbers; any other number fails the read. Errors are listed in §8.

### 7.3 Version 2: group scene

`manifest_version` 2 requires the `group` block (`GroupBlockV2`: `guid`, `title`, `subjects`, `groups`, `colour_by`, `mirror`, `show`); its fields and its writer are specified in `14` §5.1. The version-1 fields keep their meaning (the group's template space, atlas sets and backdrops).

---

## 8. Messages

Strings already quoted in §2, §4.3 and §5 are not repeated here. Backdrop messages: §11.1.7. GUI refusals: §11.1.9.

| Message | Where |
|---|---|
| `visualize: no normalization/anat/ (run normalize or add data)` | prerequisite |
| `visualize: no normalized image under normalization/anat/ (run normalize or add data)` | prerequisite |
| `visualize: missing <file> (run reconstruct first; contact QC needs reco v2 JSON)` | prerequisite |
| `dbsloc: warning: scene: atlas: Lead-DBS atlases directory: <error>` | unusable atlases directory |
| `dbsloc: warning: scene: atlas <name>: <path> ignored: <error>` | unreadable `atlas_index.mat` |
| `dbsloc: note: reconstruct: scene3d: no display available; viewer not launched (manifest written: <path>). Open it later with dbsloc viz scene --manifest <path>` | §2.3; silenced by `DBSLOC_SCENE3D_SKIP_NO_DISPLAY=1` |
| `dbsloc: note: reconstruct: scene3d: dbs-localizer not found; viewer not launched (cargo build -p dbsloc-app --release or set DBSLOC_GUI_BIN)` | §2.3 |
| `dbsloc: note: reconstruct: scene3d: viewer exited <code>`, `… viewer closed: run cancelled`, `… viewer failed to start: <error>` | §2.3 |
| `scene manifest: expected manifest_version 1 or 2, got <n>` | reading, §7.2 |
| `scene manifest: version 2 needs a group block` | reading, §7.3 |
| `manifest path <path>: <error>` | a required path is missing |
| `dbsloc: warning: scene: <path>: <error>` | a VTA image that cannot be meshed (the surface is left out) |
| `dbs-localizer scene: unknown atlas '<id>'; listed: <ids>` | exit 2 |
| `dbs-localizer scene: unknown backdrop '<id>'; listed: <ids>` | exit 2 |
| ``error: unknown navigation scheme `<name>`; use `lead-dbs` or `classic` `` | exit 2 |
| `dbsloc: error: 'dbs-localizer' not found; build from repo root: cargo build -p dbsloc-app --release, add target/release to PATH, or set DBSLOC_GUI_BIN` | `dbsloc viz scene`, exit 127 |
| *Atlas could not be loaded* window (*Copy*, *Dismiss*); *The atlas failed to load — no structures are available. See the message below.*; *No atlas configured — this scene manifest lists no atlas paths.* | viewer, *Atlas* section |
| *Reconstruction finished, but no scene manifest was written at <path>. Check that the template path is valid, then review the Output tab.* | GUI, opening the default scene |

---

## 9. Tests

- Manifest writer: tests in `crates/dbsloc-pipeline/src/stage_impl.rs` (`scene_manifest_*`).
- Manifest reader: `crates/dbsloc-viz/src/scene_manifest.rs`.
- Contact labels and atlas-set choice: `crates/dbsloc-viz/src/scene3d_gpu.rs`.
- Transparency pass, fibre ribbons and map overlays (need a GPU adapter): `crates/dbsloc-viz/src/scene3d_gpu/oit_render_tests.rs`.
- The acceptance lists of §11.1.6 and §11.1.7.

---

## 10. References

- McGuire M, Bavoil L. Weighted blended order-independent transparency. *Journal of Computer Graphics Techniques* 2(2), 2013.
- Lorensen WE, Cline HE. Marching cubes: a high resolution 3D surface construction algorithm. *SIGGRAPH* 1987.
- Taubin G. A signal processing approach to fair surface design. *SIGGRAPH* 1995.
- Ewert S, et al. Toward defining deep brain stimulation targets in MNI space: a subcortical atlas based on multimodal MRI, histology and structural connectivity. *NeuroImage* (2017).
- Alkemade A, et al. The Amsterdam Ultra-high field adult lifespan database (AHEAD): a freely available multimodal 7 Tesla submillimeter magnetic resonance imaging database. *NeuroImage* (2020).
- Edlow BL, et al. 7 Tesla MRI of the ex vivo human brain at 100 micron resolution. *Scientific Data* 6, 244 (2019).
- Horn A, et al. Lead-DBS v2: towards a comprehensive pipeline for deep brain stimulation imaging. *NeuroImage* 184, 293–316 (2019).

---

## 11. Viewer behaviour contracts

Normative for `crates/dbsloc-viz` and `crates/dbsloc-app`. Where users know Lead-DBS behaviour (mouse bindings, default view, slice defaults, contact names, the correction view) the viewers match it; Lead-DBS window objects are not reproduced. Extra features are allowed; a binding that clashes with Lead-DBS lives behind the *Classic* option (§11.1.5).

### 11.1 Scene viewer, correction view and GUI rules

#### 11.1.1 Rendering (normative, 2026-10-16)

The scene renders off-screen into its own colour and depth targets and two order-independent-transparency targets (accumulation and revealage) at the pixel size of the scene rectangle (`dbsloc_viz::SceneWgpuRenderer`, `scene_oit.rs`); one full-screen pass then composites the result over the GUI with premultiplied alpha. The off-screen work is skipped when the payload, the scene revision, the target size and the aspect ratio are unchanged since the last frame.

**Opaque pass**, in this order, depth test `Less` with depth write: electrode bodies; group lead bodies and contact markers (instanced, §11.1.4); the trajectory polyline (fallback, §11.1.2); fibre ribbons (§11.1.8); the slice quads; map slice overlays (§11.1.8). Slice quads are alpha-blended and write depth only at full opacity, so a translucent slice is tested but leaves the leads and the atlas visible through it. Each slice spans the backdrop's own grid at the plane's world position and is drawn unmirrored (superior up on coronal and sagittal planes).

**Transparency pass** (weighted blended order-independent transparency, McGuire & Bavoil 2013): atlas meshes, VTA surfaces and map isosurfaces in one indexed draw in upload order, without sorting, then dimmed group instances. Depth test `LessEqual` against the opaque depth, no depth write, both faces drawn (no culling). Accumulation: premultiplied colour · *w* in rgb, α · *w* in a (additive). Revealage: cleared to 1, multiplied by 1 − α per fragment. Weight *w* = α · clamp(8 (1 − *u*)³, 0.01, 8), *u* = clamp(0.5 + 0.5 · *d* / *r*, 0, 1), where *d* is the distance of the fragment along the camera's forward direction from the centre of the axis-aligned bounding box of the visible translucent vertices, and *r* is half that box's diagonal (at least 1 mm). Nearer fragments weigh more, and the weight does not depend on the camera distance.

**Composite:** colour = (Σ premultiplied colour · *w* / Σ α · *w*) · (1 − *R*) + opaque colour · *R*; alpha = (1 − *R*) + opaque alpha · *R*, with *R* the revealage.

Consequences: the result does not depend on the order of the structures or their triangles, so nested or touching structures (GPi inside GPe, STN next to SN) do not flicker while the camera turns, and a camera move needs no CPU work on the atlas. A single translucent layer composites exactly as ordinary alpha blending; overlapping layers give a weighted average rather than a strict front-to-back blend. A lead behind a structure is tinted by it; a lead in front stays opaque. The per-structure α multiplies the global atlas *Opacity*.

#### 11.1.2 Electrode bodies, trajectory fallback and contact labels (2026-10-15; labels 2026-11-14)

**Lead body** (`electrode_mesh.rs`, `fn build_electrode`). Built from the catalogue geometry as `09` §11 specifies (shaft, contact bands and directional pads, tip; no Lead-DBS mesh files) and placed by the marker map `T` of `05` §6.3: rotation, translation and a stretch `s = |tail − head| / fiducial_span` along the lead only. All axial lengths scale by `s` while the radius and the segment angles stay physical; the body is never bent along the trajectory and never fitted to `coords_mm`. In native space `s ≈ 1`; in template space `s` follows the warped head and tail (Lead-Tutor cohort: 1.07–1.31), and the contact bands land on the template-space contacts. The shaft ends where `09` §11 says: at the most proximal trajectory sample, and at least one contact length plus 1 mm beyond the last level centre. A body is drawn only when the side's model is in the catalogue and the side has `head` and `tail` markers; with `x` or `y` missing the frame is completed right-handed.

**Trajectory polyline (fallback only).** Drawn only for a side without a body (model not in the catalogue, or no markers), so a lead is never invisible; never drawn next to a body. Lead-DBS draws no reconstruction line in its 3D view, and the 50-point line starts 5 mm below the head (`05` §5), so it would stick out of the body. Opaque, with depth test and depth write. There is no user toggle; a saved `show_trajectories` from an older version is ignored.

**Contact markers and labels** (*Electrode* section, *Mark and number contacts*, off by default; `scene3d_gpu.rs`, `fn contact_labels`, `fn paint_contact_labels`). Each point of the side's `coords_mm` gets a disc and a label `<name> (R)` or `<name> (L)`, `<name>` being the catalogue contact name of `09` §2.3 for that side: for example `K0 (R)` … `K3 (R)` and `K8 (L)` … `K11 (L)` for a Medtronic 3389, `K2A (R)` for a directional segment. When the model is not catalogued, or its name count differs from the number of points, the labels are `R1`, `R2`, … and `L1`, `L2`, … by position in `coords_mm`. A label is placed right of its disc, else left; a label that would overlap an earlier one or leave the scene is not drawn. The section shows `<n> left / <m> right contacts`, or `No contact coordinates in this reconstruction`.

#### 11.1.3 Side panel, keyboard and scene image

The side panel holds these sections, in order (`scene3d_gpu.rs`):

| Section | Controls |
|---|---|
| *Slices* | *Backdrop* combo (§11.1.7); *Axial slice*, *Coronal slice*, *Sagittal slice* with *Index* (`Index <n> · <x|y|z> <mm> mm`) and *Opacity* |
| *Window / level* | *Window* (0.05–1), *Level* (0–1); *Full range* (1.00 / 0.50), *Soft tissue* (0.45 / 0.35), *Bone / metal* (0.35 / 0.80) |
| *Electrode* | *Show lead bodies* (on), *Mark and number contacts* (off) |
| *Stimulation* | *Show VTA* (on); the loaded label and sides, or *No stimulation loaded* |
| *Fibres*, *Maps* | §11.1.8 |
| *Group* | group scene only, §11.1.4 |
| *View* | *Orthographic* / *Perspective*; *Distance (mm)* (20–2000, logarithmic), *Elevation (°)* (±89), *Azimuth (°)* (±180); *⟳ Reset view*; *Mouse & touchpad* with the *Scheme* combo and the active scheme's bindings |
| *Atlas* | *Show atlas overlay*, *Opacity*, *Atlas set* (§5.4), *Preset* (*Show all structures*, *Hide all structures*, then the index presets; *Custom selection* after a manual change), *Structure names* (when the index has label sets; *File names* first), `<n>/<N> shown`, and the *Structures* list (tick box, colour and opacity per structure; a spinner while a surface is prepared) |

*Reset view* restores the default camera of §11.1.5 (the projection is kept), the default slices of §11.1.7, slice opacities 1 and window / level 1.00 / 0.50.

**Keyboard** (active while the pointer is over the viewer or the viewer was clicked last; plain keys are ignored while a text field has focus):

| Key | Action |
|---|---|
| `R` | reset the view |
| `1` / `2` / `3` | show or hide the axial / coronal / sagittal slice |
| `Up` / `Down` | previous / next atlas structure |
| `Space` | show or hide the focused structure |
| `Shift` + arrows | pan by 5 % of the camera distance |
| `Ctrl+S` | save the rendered scene to an image |
| `F1` | show or hide the *Scene controls* help (mouse bindings of the active scheme and this table); `Esc` closes it |

**Scene image.** *Save scene* (`Ctrl+S`) opens *Save scene image* with a path (default `<home>/<subject>_scene3d.png`); the extension must be `.png`, `.jpg` or `.jpeg` (*⚠ Give the file a .png or .jpg extension.*), and an existing file is replaced after the warning *⚠ This file already exists and will be replaced.* The image is the scene rectangle only, without the controls, written on a worker thread; the top bar then shows *✔ Saved to <path>* with *Copy path* and *Open folder*.

**Software rendering.** On a software adapter (Addendum A) the atlas starts hidden, with a note in the *Atlas* section, because drawing it on the processor takes seconds per frame. This automatic hiding is not saved: the saved view keeps *Show atlas overlay* on unless the user turned it off.

#### 11.1.4 Group scene

A version-2 manifest (§7.3) draws every patient's leads in the group's template space. The rendering rules (colour by group or by a clinical variable, active and passive contacts, VTAs, legend, highlight, hover text `sub-<id> · group · side · value`, double click centres on a lead), mirroring and performance targets are specified in `14` §5. The viewer side (`group_scene.rs`): one lead mesh per electrode model, instanced per lead (`electrode_mesh.rs`, `fn lead_instance_matrix`, the placement of §11.1.2); contact markers as instanced spheres (radius 0.9 mm, inactive contacts 0.45 mm, 0.8 mm points when colouring contacts by value); dimmed leads and VTAs go through the transparency pass (§11.1.1); VTA meshes are cached by image content under `<cache>/vta_meshes/` (§5.3); the default camera target is the mean of all contacts. The *Group* section offers *Show active contacts*, *Show passive contacts*, *Highlight active contacts*, *Show volumes of tissue activated*, *Colour contacts by value*, *Highlight patient* and *Highlight group*; the *Group analysis* tab drives the same settings.

#### 11.1.5 Scene navigation (mouse) — behaviour contract (2026-10-14)

Lead-DBS installs its own figure callbacks for the scene camera (button down, motion and scroll wheel; Lead-DBS: `ea_mouse_camera`). It does not use MATLAB's built-in rotate or camera-toolbar modes, has **no keyboard camera control** and **no preference** for the bindings. The scene camera is orthographic in the stored default view; the orbit centre is the camera target, which only pan and the default-view reset move.

| Input | Lead-DBS effect | Gain |
|-------|-----------------|------|
| Left drag | Orbit about the camera target; vertical axis fixed to world **z** (superior) so the head never rolls; horizontal drag = azimuth, vertical drag = elevation | **1° per 7 px** of pointer travel, each axis |
| Right drag, or Ctrl + left drag | Pan: camera and target move together in the view plane, the scene follows the pointer | **half** the pointer distance |
| Middle drag, or Shift + left drag | Zoom (view angle); drag **right or up** = zoom in | factor `1 + clamp((dx + dy_up) / 70, −0.9, 0.9)` per motion event |
| Mouse wheel | Zoom toward the view centre (not toward the pointer); wheel **toward the user** (scroll down) = zoom **in** | factor `1 + n / 10` per notch in, `1 / (1 + n / 10)` out |
| Double click (any button) | Reset to the saved default view (camera, slices) | animated in Lead-DBS |
| Keyboard | none for the camera | — |

**Wyss DBS Localizer (`dbsloc_viz::gesture_zoom::NavigationScheme`):** two schemes, selectable in the viewer's *Mouse & touchpad* section and persisted by the GUI (`DbslocAppState.navigation`).

- **`LeadDbs` (default, label *Lead-DBS*):** the table above. Zoom acts on the orbit distance: a Lead-DBS zoom factor `q` divides the distance. Wheel: one notch (40 points) toward the user divides the distance by 1.1; one notch away multiplies it by 1.1. Drag zoom: `q` from the table with egui's screen delta (`dy_up = −delta.y`). Pan: half the pointer-follow gain of the `Classic` scheme. Orbit: 1° per 7 points regardless of viewport size. Double click (any button) = *Reset view* (same as key `R`, not animated). Touchpad pinch / Ctrl + wheel still zoom (`zoom_delta`) because touch devices have no middle button; the keyboard shortcuts of §11.1.3 stay.
- **`Classic`:** the Wyss bindings in force before 2026-10-14: left drag orbit (a full viewport height sweeps 180°), right / middle / Shift + left / Alt + left drag pan (scene follows the pointer), Ctrl + right drag zoom, wheel zoom with wheel **away** from the user = in, touchpad two-finger scroll pan, pinch / Ctrl + wheel zoom.

The help window (F1), the *Mouse & touchpad* section and the scene hover text describe the **active** scheme only. `dbs-localizer scene` defaults to `LeadDbs` (`--navigation`, §4.1).

**Camera model (2026-10-14).** The orbit camera is a z-up spherical camera, as in Lead-DBS: `azimuth` (°) is the horizontal rotation about world **z** in MATLAB's `view(az, el)` convention (0° = camera on −y looking anterior, 90° = camera on +x, 180° = camera on +y looking posterior); `elevation` (°) is the angle above the xy-plane, clamped to ±89°; the camera up vector is world z. Eye = target + distance · (sin az · cos el, −cos az · cos el, sin el). The *Azimuth (°)* and *Elevation (°)* sliders show exactly these two angles.

**Projection.** Two projections, `Orthographic` (default, as in Lead-DBS) and `Perspective` (45° vertical field of view), selectable in the *View* section and persisted by the GUI. In both the `distance` value is the zoom: the visible half-height at the target plane is `distance · tan(22.5°)`, so a given distance frames the same height in either projection, and pan / dolly gains do not change. In the orthographic projection the eye is placed far from the target (3000 mm along the look direction), so nothing between the real camera distance and the target is clipped.

**Default view (Lead-DBS default view of MNI152NLin2009bAsym; *Reset view* restores it):**

| Parameter | Lead-DBS | Wyss DBS Localizer |
|-----------|----------|--------------------|
| Azimuth | 180° | 180° |
| Elevation | 21° | 21° |
| Projection | orthographic, view angle 9.26° at 490 mm = 79.3 mm visible height | orthographic, `distance` = 79.3 / (2 · tan 22.5°) ≈ **96 mm** |
| Target (orbit centre) | (−3.16, −25.27, −3.65) mm | the same point in world mm when it lies inside the template volume, else the template centre |
| Up | world z | world z |

Slice defaults: §11.1.7. Persisted `SceneViewState` snapshots carry `camera_convention = 1`; a snapshot without it (written before this change) restores atlas and contrast but not the camera, which falls back to the default view.

#### 11.1.6 Refine localization view — behaviour contract (2026-10-14)

The expert's correction tool of `05` §9a. Reference behaviour (`12` Part 2): slices of the post-op image with the lead drawn on them; the user picks contact 0 (head) or the tail, nudges it with the arrow keys, rotates the lead and finishes a side with Space. The Wyss view keeps that model.

**Component.** `dbsloc_viz::refine_app::RefinePanel` (feature `refine-embed`, library only; the GUI embeds it in the *Refine localization* tab, pop-out with F11 like the other viewers). Input `RefineRequest` (serde): `subj_id`, `space` (`native` | `mni`), `backdrop_nii` (the image of `05` §9a *Edit space*), `sides: [{key: "1" | "2", label: "Right lead" | "Left lead", elmodel, head, tail, x, y, roll_deg, candidates_deg, orientation_status, manually_corrected, review}]` (`review` = `"corrected"`, `"accepted"` or `null`, `05` §9a *Per-side review*), and `warnings` (the tail conversions of `05` §8.8, shown on the status line) — coordinates in the edit space, millimetres. The pipeline builds it (`manual_correction::prepare_manual_correction(sub, opts) -> ManualCorrectionContext`, same field names); the panel never reads a subject folder. Output `RefineOutput { save: Option<Vec<SideEdit {key, head, tail, roll_deg: Option<f64>, accept: bool}>>, cancelled, errors }`: the host applies the edits with `apply_manual_correction` off the paint thread; the panel never writes files. `roll_deg` is `Some` only when the user changed the roll. `accept: true` marks a side accepted without an edit (`05` §9a); its `head` / `tail` are the file's markers and are not read by the pipeline.

**Layout.** Three slice panes of the backdrop (axial on the left half, coronal and sagittal stacked on the right; voxel-aligned planes of the backdrop grid, physical aspect, same pane mapping as the registration review) plus a controls column; `1` / `2` / `3` show a single pane, `0` restores the three-pane layout. Window / level, zoom and pan behave as in the registration review (Shift + left drag = window / level, right drag / pinch / Ctrl + wheel = zoom, middle or Alt + left drag = pan, `R` = reset zoom / pan). Default window for a CT backdrop: level 300 HU, width 1500 HU (bone / metal); for an MR backdrop the 0.5–99.5 percentile range.

**Overlay (egui paint over each pane, from the live geometry of `05` §5 `resolve_fiducials` of the current markers):** the lead axis (the 50-point trajectory projected into the pane), every contact centre as a circle of the contact radius at the pane scale, the **head** and **tail** handles as larger rings, the selected one filled with the accent colour; the segment-A direction (`y − head`) as a 5 mm arrow from the head in the pane; the other side drawn dimmed, not selectable. Points farther than 2 mm from the pane's slice are drawn at half opacity. A status line under the panes: `head shift`, `tail shift` (mm from the file's markers), `roll` (°, and `Δ` from the file), `tilt from vertical` (°), electrode model.

**Crosshair / slices.** The three panes show the slices through a crosshair. Selecting a marker, nudging it or dragging it moves the crosshair onto that marker; the wheel steps the hovered pane's slice away from it to inspect; `F` recentres on the selected marker. On open, after a side switch and on every recentre the panes zoom so that about 50 mm of in-plane extent fills the shorter pane side around the crosshair (the lead, not the whole head); the user's zoom is kept until the next recentre, and *Reset zoom and pan* (`R`) returns to zoom 1.

**Editing (per side, the current side chosen with `<` / `>` or the side tabs):**

| Action | Input |
|--------|-------|
| Select head (contact 0) / tail (the proximal fiducial) | `H` / `T`, or click within about 12 points (0.85 em) of the handle in any pane (`0` and `1`–`3` stay the layout keys, as in the registration review) |
| Nudge the selected marker in the hovered pane's plane | `←` `→` `↑` `↓`: 0.25 mm; Shift = 1 mm; Ctrl = 0.05 mm (pane axes, so `↑` on the axial pane is anterior) |
| Move the selected marker through the pane plane | `PageUp` / `PageDown`, same steps |
| Drag the selected marker | left drag on its handle; through-plane coordinate unchanged |
| Move the whole lead (head and tail together) | the same keys with Alt, or dragging the lead axis between the handles |
| Roll | `[` / `]` = −1° / +1°, Shift = 10°; slider and numeric field in the controls; *Flip 180°*; *Other candidate* (enabled when `candidates_deg` has two values); *Automatic* (back to the file's `roll_deg`) |
| Numeric entry | head and tail `x`, `y`, `z` and roll fields in the controls (mm, 0.01 steps) |
| Undo / redo | `Ctrl+Z` / `Ctrl+Y` (per side) |
| Reset side | *Reset side* button (markers and roll of the file) |
| Accept side and go to the next | `Space` (*Accept side*): marks the side as checked; an edit of the side afterwards clears the mark |
| Save | *Save corrections* button (`Ctrl+Enter`): emits `save` for every side whose markers or roll changed, with `roll_deg` only when the roll changed, and an `accept: true` entry for every side accepted without an edit; a side that was edited and accepted is saved as an edit |
| Discard | *Discard* button (`Esc` when nothing has focus): `cancelled` |
| Centre on the selected marker | `F` |

Plain keys are ignored while a text field has focus (`ctx.text_edit_focused()`, so a focused button does not swallow them), as in the 3D scene; the table is exported by `refine_app::shortcut_rows()` for the F1 help and listed in `docs/agent-rules/gui-ux.md`.

**Rules.** A side whose electrode model has ring contacts only (`is_directed == false`) shows no roll controls (one muted line *Ring contacts: roll not applicable*), never emits `roll_deg`, and its status line omits the roll. A side's tab adds *· corrected* when the file has it corrected, *· accepted* when the file has it accepted, *· edited* while it has unsaved edits, and *· accepted, not saved* for an accept not yet saved; an unsaved accept counts as a change for *Save corrections* and the discard question. The panel draws no heading of its own (the host shows the subject). The roll is kept when the head or tail moves (the panel re-derives `y` by projecting the previous `y − head` ⟂ the new axis, exactly as `05` §9a step 2, so what the panel draws is what the pipeline will write). A move that would make `head == tail` is refused (no change). Marker coordinates are never rounded by the panel. Both sides may be edited before one save. After a save the host shows a *Manually corrected* chip on the Reconstruct card, one line per lead with its review state (*corrected*, *accepted*, *unchecked*, or *not found* for a side the reconstruction left out, `05` §4.4) and *vertical segment marker* when `meta.reconstruct.flags` carries `vertical_marker` (`05` §8.6, `06` §10.6), reloads the 3D scene, and warns on that card that *Rerun all selected steps* traces every lead again and drops the manual corrections and acceptances.

**Acceptance.** (a) Pane mapping round trip: a world point → pane uv (`plane_uv_for_world(plane, world, slice_index)`, with the signed through-plane distance in mm) → world point (`world_for_plane_uv(plane, u, v, slice)`) within 1e-4 mm on an oblique backdrop affine. (b) `→` on the axial pane moves the selected marker by (+0.25, 0, 0) mm in the backdrop's voxel `i` direction mapped to world (for a RAS-aligned affine: +x); Shift / Ctrl scale it. (c) After moving the head 3 mm along the lead axis, or with a roll set, the roll reported by the panel equals the previous roll within 1e-6° (a lateral move changes the axis, so the projected roll changes geometrically, as in `05` §9a step 2). (d) `]` ten times = +10° roll; *Flip 180°* twice = identity. (e) `save` lists only the edited sides, with `roll_deg: None` when only markers moved; a side accepted without an edit is listed with `accept: true`, an edited and accepted side with `accept: false`. (f) `Ctrl+Z` after one nudge restores the previous markers exactly. (g) `DBSLOC_APP_SCREENSHOT_ACTION=refine` with `DBSLOC_APP_REFINE_REQUEST=<RefineRequest json>` over synthetic data renders the tab (never a real subject).

**Lead-DBS parity amendments (normative, 2026-10-22; reference behaviour `12` Part 2, gaps of its §12 closed: 3, 4, 5, 8, 9, 10, 13, 14, 17, 19, 21).** The *Lead planes* view is the default (`L`): two panes that contain the lead axis, **Lead from the front** (axis + world x) and **Lead from the left** (axis + world y), resampled trilinearly from the backdrop along the current trajectory (head − 5 mm to tail + k·catalogue span, k of `09` §2.2; ±10 mm across; 0.2 mm steps; resampled only when the markers change, synchronously — 512³ CT: ≤ 5 ms, ≤ 11 ms with the slab), the lead axis pointing up, shown side by side or stacked, whichever gives more pixels per millimetre; beside them **Head, axial** and **Tail, axial** close-ups (±15 mm in the backdrop's own i/j plane through each marker; each close-up offers only its own marker for selection); the three voxel-aligned panes of the layout above become the *Three planes* view (`0`) and the single-pane views (`1`–`3`). Every pane zooms and pans; R/L, A/P (and S/I on the orthogonal panes) captions sit at the image edges. The lead panes and the close-ups draw only the current side. Dragging a handle in a lead pane freezes that pane's plane until release so the handle follows the cursor; the other panes follow live. Arrow keys in a lead pane move across the lead (`←` `→`) and along it (`↑` `↓`), `PageUp` / `PageDown` through the plane. **Keep lead length** (checkbox, `K`, default on): an in-plane nudge or drag of one marker recomputes its coordinate along the lead axis so that |tail − head| is unchanged (the moved marker pivots about the other and stays on the same side of it; a move longer than the lead stops at the lead length with a status note; a move purely along the axis is refused with a note pointing to the along-axis move; typed coordinates ignore the option). **Move the whole lead along its axis:** `Alt+PageUp` (proximal) / `Alt+PageDown` (distal), 0.25 mm, Shift 1 mm, Ctrl 0.05 mm, roll unchanged (`Alt` + arrows still move the whole lead in the hovered pane). **Move whole lead** checkbox beside the numeric head / tail fields: editing one marker translates the other by the same vector. **X-ray (slab)** (`X`): the lead panes show the mean of 9 parallel planes at −4 … +4 mm along the pane normal; display only, saved markers unchanged. **Hide markers** (`V`): hides handles, contacts, axis and the segment-A arrow in every pane; keys still act. The status line adds `Spacing: s mm (catalogue c mm, ratio r)` = the mean adjacent level distance of the marker-implied contacts (the stretched construction of `05` §6.3, so it tracks the head–tail distance); an edit never touches the other side. A static model diagram (catalogue levels, *Head (contact 0)* and *Tail* labelled) is shown when the controls column has spare height. Window / level and zoom controls live in a collapsed section; *Save corrections* and *Discard* are pinned at the bottom; the view fits a 1280×720 pop-out without scrolling. **Roll convention:** the panel's roll is the horizontal azimuth of `06` §10.6 (`12` §11.5): x̃ = (cos θ, sin θ, 0) projected ⟂ the lead, y_dir = t × x_dir, θ = 0 → y anterior; read-back = the unsigned angle between anterior and the horizontal projection of y − head, negative when y lies on the +x side of head; a vertical lead is unchanged, a 30° lateral tilt at θ = 45° differs by 4.1° from an axis rotation. `SideEdit.roll_deg` and the request's `roll_deg` / `candidates_deg` are in this convention (the pipeline writes the same convention, `05` §9a step 2). Acceptance (`12` §13 items 1–7 as `refine_app::tests`): a 30°-tilted bright cylinder appears as a straight band along the lead panes; keep-lead-length holds within 1e-9 mm over 20 random nudges; the along-axis move shifts head and tail by the same vector; the spacing readout equals the catalogue spacing for a catalogue-length lead and grows 10 % with a 10 % stretch; the roll convention agrees with the azimuth formula within 1e-6° for a vertical lead; slab mode and the hide toggle leave the markers byte-identical.

#### 11.1.7 Anatomy slices and backdrop — behaviour contract (2026-10-22)

Reference behaviour (Lead-DBS *Anatomy Slices* control and its default view of MNI152NLin2009bAsym): the three orthogonal slices show one **backdrop** image; the default slice position is a fixed point of the space, not derived from the leads; a popup lists the backdrops (the space's templates, the patient's images, downloaded high-resolution backdrops, *Choose…*).

**Default slices.**

| | Reference | Wyss DBS Localizer |
|-|-----------|--------------------|
| Position (world mm) | x = −30, y = −30, z = −30 | the same, in every template space |
| Shown | sagittal, coronal | sagittal, coronal (axial hidden) |
| Opacity | 100 % each | 1.0 each |

The slice position is a world-mm point; each plane's voxel index in the current backdrop is `round(world_to_voxel(p))` on its axis, clamped to the volume. When the point lies outside the backdrop's extent on an axis (a cropped backdrop), that plane uses the backdrop's centre index ⌊n / 2⌋ on that axis. The viewer keeps the exact world position of each plane (`slice_world`), not only the index: it changes only when the user moves that plane (slider, keyboard) and is then set to the plane's world coordinate in the current backdrop; switching backdrop, saving and restoring map this stored position, so a plane that falls back to the centre of a cropped backdrop returns to its old position when the user switches back, and repeated switches do not drift by grid rounding. *Reset view* restores these defaults. The side panel shows each plane's world coordinate next to its index (e.g. *Index 104 · y −30.0 mm*).

**Persistence.** `SceneViewState` stores the slice positions as world mm (`slice_mm: [x, y, z]`), the three visibility flags, the opacities and `backdrop_id`, with `slice_convention = 1`. A snapshot without `slice_convention` (written before this change) restores camera, atlas and contrast but not slice positions or visibility, which take the defaults above. Restored world positions map to the current backdrop's grid with the rule above, so they survive a backdrop change.

**Reference template vs backdrop.** The manifest's `template_nii` stays the scene's reference image: atlas meshing, scene bounds, default camera target and the mesh cache key use it, never the backdrop. The backdrop is only what the slices show. The slice quads span the backdrop's own grid (a cropped backdrop draws smaller planes).

**Backdrop list** (combo *Backdrop* at the top of the *Slices* section), in this order:

1. **Template images** of the scene's template: `<space> T1w (template)` = `template_nii`; `<space> T2w (template)` when `t2.nii.gz` or `t2.nii` exists beside it. Kind `template`.
2. **Patient images in template space**, each listed only when its file exists (`01` §7a names, `derivatives.rs`): `Patient post-op CT (normalized)` (`norm_postop_image(sub, "CT", space)`), `Patient post-op MRI, axial / coronal / sagittal (normalized)` (keys `ax`, `cor`, `sag`), `Patient pre-op <modality> (normalized)` (`norm_anchor_image(sub, space)`). Kind `patient`.
3. **Installed downloadable backdrops** of the scene's space (catalogue below). Kind `download`. In the GUI, catalogue entries of that space that are not installed are listed too, labelled `<title> — download (NN MB)`.
4. **Choose file…** (GUI only): any NIfTI file, which must already be in the scene's template space; kept as kind `custom` with id `custom:<absolute path>`.

The list is built by one pipeline function `dbsloc_pipeline::backdrops::scene_backdrops(sub, template_t1, space) -> Vec<BackdropOption { id, label, path, kind }>` (ids: `template-t1`, `template-t2`, `patient-postop-ct`, `patient-postop-ax` / `-cor` / `-sag`, `patient-preop`, catalogue id for downloads). The manifest writer (§2.2) stores it as `backdrops: [{ "id", "label", "path", "kind" }]` (absolute paths; optional field); the GUI calls the same function when it loads a scene, so manifests written before this change also get the full list. `dbs-localizer scene` uses the manifest list, or `template_nii` alone when the field is missing.

**Default backdrop:** `template-t1`. The persisted `backdrop_id` is used when that id is in the list; otherwise `template-t1`. **Atlas set:** the persisted `atlas_id` follows the same keep-the-choice rule (§5.4).

**Switching.** Choosing an entry loads the image on a worker thread (*Loading backdrop…* with a spinner under the combo); camera, atlas, slice world positions, visibility and opacities are kept; window / level returns to its default (1.0 / 0.5) because the new image has its own intensity range. A failure (unreadable file, loader dimension guard, wrong file) shows one error line under the combo and keeps the previous backdrop. A backdrop whose world extent does not overlap the template's shows the warning *This image does not overlap the template; is it in <space>?* but is drawn. A backdrop whose voxel axes are not in x / y / z order (for each voxel axis i, the largest absolute sform / qform component of its column must lie on world axis i; flips allowed) is refused with *This image's axes are not in x / y / z order; resample it to the template first.* and the previous backdrop is kept. At most one backdrop load runs at a time: a choice made while a load runs is queued (a later choice replaces the queued one) and starts when the running load ends; its result is dropped when another choice was made meanwhile. Choosing the backdrop already shown while a load runs cancels the queued choice. A restored `backdrop_id` that is not yet in the list (or still loading) is what the next save writes, so a pending choice is never overwritten by `template-t1`. At most one backdrop volume besides the reference template is kept after a load completes (the template T1 backdrop shares the reference volume). Intensity normalisation: 0 → the 99.5th percentile of the non-zero voxels.

**Backdrop catalogue** (`crates/dbsloc-pipeline/src/backdrop_catalog.rs`, the only place naming backdrop sources and holding their pinned source URL, size and SHA-256; fetch `backdrop_fetch.rs`). Every download is verified against its pins. Entries for MNI152NLin2009bAsym, both from OpenNeuro ds002179 (synthesized FLASH 25° images in MNI space):

| id | Title | Install |
|----|-------|---------|
| `edlow2019-7t-exvivo_res-0.5mm` | 7T ex vivo brain (Edlow 2019), whole brain, 0.5 mm | copied verbatim |
| `edlow2019-7t-exvivo_res-0.2mm_desc-deepbrain` | 7T ex vivo brain (Edlow 2019), deep brain region, 0.2 mm | cropped to x ∈ [−50, 50], y ∈ [−60, 35], z ∈ [−40, 35] mm |

The 0.1 mm file (7.3 GB) is not offered: it exceeds the loader guard and typical free disk. Lead-DBS's "100 µm" backdrop is the 0.2 mm whole volume read memory-mapped; Wyss offers the whole brain at 0.5 mm and the deep brain region at 0.2 mm instead, because the viewer holds its volume in memory. Data licence: CC0 (Dryad and OpenNeuro records). Citation, written to the installed `LICENSE` and shown in the GUI download row: *Edlow BL, Mareyam A, Horn A, et al. 7 Tesla MRI of the ex vivo human brain at 100 micron resolution. Sci Data 6, 244 (2019), doi:10.1038/s41597-019-0254-8; data: OpenNeuro ds002179, doi:10.18112/openneuro.ds002179.v1.1.0.* The OpenNeuro mirror is used because the Dryad API refuses unauthenticated downloads.

**Fetch** follows `04` §3.2 (HTTPS GET streamed to a staging folder with incremental SHA-256, progress at most every 1 MB, cancellation between chunks, body limited to the pinned size + 1 MB, size and SHA-256 must equal the pins, atomic rename onto the destination, old copy kept aside until the new one is in place, staging removed on failure or `cancelled`), with these differences:

- Staging `<data>/backdrops/space/<space>/.partial-<id>-<pid>-<n>/download.nii.gz` (`n` a per-process counter, so two fetches in one process never share a folder); a fetch first removes `.partial-<id>-*` folders of that space older than 24 hours (left by a killed process); destination `<data>/backdrops/space/<space>/<id>/` with `backdrop.nii.gz` (`backdrop.nii` for a cropped entry), `LICENSE` (licence and citation text from the catalogue) and `backdrop.json` (`id`, `space`, `title`, `res_mm`, `source_url`, `sha256`, `crop_mm` or `null`, `fetched_by`). Installed = the backdrop file exists.
- Before the download, free space on the data folder's volume must be at least pinned size + expected output size + 512 MB; otherwise fail with `not enough free disk space for <id>: need <N> MB, <M> MB free` before any byte is fetched.
- **Crop** (entries with a crop box): the downloaded file is stream-decompressed once; the header must have the pinned dimensions and datatype float32 and an axis-aligned positive-diagonal sform (else fail, nothing installed). The voxel range on each axis is `ceil((lo − origin) / pixdim)` … `floor((hi − origin) / pixdim)`; only the z slabs in range are kept, each cropped in x / y; the output is float32 with the same voxel size, sform and qform translated to the first kept voxel (codes kept), `scl_slope` / `scl_inter` copied, written **uncompressed** as `.nii` (the float crop barely compresses and the uncompressed file loads faster; an earlier `.nii.gz` install is still read). The download is deleted after the crop.
- CLI: `dbsloc backdrop list` (id, space, title, size, state) and `dbsloc backdrop fetch <id>` (progress lines, Ctrl-C cancels, exit 0 / 1 / 130).

**GUI.** The 3D viewer's combo lists the entries above. Choosing an entry that is not installed shows, in the scene panel, the title, size, citation and *Download (NN MB)*; then a progress bar with *Cancel*; on success the backdrop is selected. The download runs on a worker thread; closing the application cancels it. The chosen `backdrop_id` is saved with the view state.

**Acceptance.** (a) Fresh view on the shipped 1 mm template: sagittal index 68, coronal 104, axial 42 (x = y = z = −30 mm), sagittal + coronal shown, axial hidden. (b) The same position on the 0.5 mm Edlow grid: indices 136, 208, 84. (c) Switching backdrop keeps the camera and the slice world positions; a snapshot without `slice_convention` gets the defaults. (d) Crop of a synthetic float32 volume (known values, known origin) yields the expected dims, sform translation and voxel values; a header with other dims is refused. (e) Catalogue pins non-empty; fetch refuses a size / SHA-256 mismatch and cleans the staging folder (local test server or file-backed test hook, no network in tests). (f) `scene_backdrops` lists exactly the existing files in the order above.

#### 11.1.8 Fibre and map layers — behaviour contract (2026-10-25)

Owner decisions of 2026-10-25. Both layer kinds are pushed by the application (never read from the scene manifest) and are not persisted; the fibre width and the maximum fibre count are part of the saved view state (`SceneViewState.fibre_width_px`, `max_fibres`).

- **Fibre layer** (`scene_layers.rs`, `FibreLayer`) = id, label, the fibres (world-mm points in template space, per-fibre offsets), one value per fibre, a colouring and the total fibre count N of the source. Sources: the connected fibres of a stimulation (`15` §7.4 / §9.2 `_fibers.mat`, stimulation editor *Show fibres*; colouring *strength*: white → `[0.9176 0.2 0.1373]` over 0 … max |value|), a fibre-filtering result (`14` §8.6 `fibres_R/L.mat`, *Show in 3D*; colouring *signed*: the `14` §8.3 positive / negative gradients, white at the smallest |value| of the shown fibres of that sign and the peak colour at the largest, so a result that keeps only the top fibres still spans the full gradient; the side panel shows the range), and any fibre file the user adds (*Add fibre file…*, `.mat` of `15` §3.2 or `.trk`, uniform colour, chosen per layer).
- **Drawing.** Fibres are screen-facing ribbons of constant pixel width (setting 1–6 px, default 2 px, scaled by the display's pixels per point), one colour per fibre, opaque, depth-tested and depth-writing, drawn in the opaque pass after the electrodes and before the slices (§11.1.1); translucent VTA, atlas and map surfaces therefore tint the fibres behind them. Fibre geometry has its own vertex buffer and generation: adding, removing or re-colouring a fibre layer never re-uploads atlas or electrode meshes.
- **Decimation.** Per layer at most *Maximum shown* fibres (default 200, slider up to 100 000), chosen by descending |value|, ties by ascending fibre index (`15` §7.4); the panel shows `<n> of <N> fibres shown` per layer.
- **Side panel.** A *Fibres* section lists the layers (visibility, colour swatch for uniform layers, `<n> of <N> fibres shown`, remove) with the shared *Width* and *Maximum shown* controls and *Add fibre file…* (only when the hosting application supplies a reader; hidden in `dbs-localizer scene`).
- **Map layer** (`MapLayer`) = id, label, a template-space scalar volume (dims, affine, values) and a display mode. Default mode *Isosurface*: positive and negative parts meshed at ± threshold after light smoothing, coloured with the `14` §7.7 peak colours, translucent through the transparency pass (α 0.5), meshed on a worker when the threshold changes. Alternative mode *Slices*: the values painted on the visible anatomy slices with the `14` §7.7 two-sided gradient, voxels below |threshold| transparent; only the first map in this mode is drawn. The threshold slider spans 0 … max |value| (default 25 % of max |value|). A *Maps* section lists the map layers (visibility, mode, *Threshold*, remove). Group explorer results use the layer ids `ss:<id>:<R|L>` (sweet spots), `ff:<id>:<R|L>` (fibre filtering) and `nm:<id>` (network mapping) (`14` §11a).

#### 11.1.9 GUI locks — runs, image import and the stimulation editor (2026-11-12)

Normative for `dbsloc-app`. Every refusal named below shows its reason in an error window (`docs/agent-rules/gui-ux.md` *Error display*), not only in the log, unless stated otherwise; where a control is said to be disabled, the reason is also its disabled-hover text.

- **Run → subject and output directory.** While a run is active the dataset and the selected subject cannot change (`Ctrl+O` and `Alt+↑` / `Alt+↓` during a run only write the reason to the log); while a run, the registration review, *Refine localization* or the stimulation editor holds results of the output directory, the output directory cannot change.
- **Starting a run.** A run is refused, with the reason in a *Cannot start the run* window, while a run or a run of several subjects is active, while a wait reason stands (review images being prepared, the clean-up after a registration review still running, a lead correction being opened with the *Refine localization* button (not one the tab opens while following the selection), unsaved or being saved, an image import writing), or when no subject is selected. This holds for every way to start one: *Run* (one or several subjects), `Ctrl+R`, the overwrite confirmation, the guided *Run this step*, *Rerun this step*, *Open 3D viewer* and *Compute VTA* (the latter in a *Cannot compute the VTA* window); a run of several subjects refused for an unmet setting or an active batch reports it in a *Cannot start this run yet* window. `Ctrl+R` does nothing on the start screen and while the import window is open. No refused run moves the focus to the *Output* tab. Preparing a registration review while a run is active, or while review images are still being prepared, is refused in a *Registration review* window.
- **Image import ↔ jobs that write into a subject (`01` §8.4).** The *Import images* wizard does not open while any of these is active, each with its own reason: a run; a run of several subjects (including its pending reviews); a registration review being prepared or open; the clean-up after a registration review still running in the background (the decision itself is written when *Approve* / *Reject* is pressed); *Refine localization* being opened with its button, or open; a lead correction being saved; the group analysis *Compute VTAs* job, counted until its worker thread has really ended (also after *Close* in the group panel, and also when it panicked). The GUI jobs left out of this lock, and why, are listed in `01` §8.4: stimulation *Save* and *Compute connectivity*, the group *Analysis* jobs and the session-report import write neither raw images, `rawimages.json` nor stage outputs, approvals or locks. The header item *Import… → Images into a subject…*, the start-screen button and the guided Subject-step button are disabled; `Ctrl+I` shows the reason in an error window. While the wizard is open the same check runs every frame: *Import* and *Import anyway* (in the no-post-op question) are disabled, and if an import is still requested (a state that changed in the same frame) it is refused with the reason in the wizard's error window, before the no-post-op question opens; nothing is written and the wizard keeps every choice. Conversely, while the import job writes into the dataset: *Run* is disabled and a run started another way is refused; a batch does not start and its next subject waits until the import has finished; preparing a registration review, opening *Refine localization*, saving a lead correction and the group *Compute VTAs* are refused with "Wait until the image import has finished writing into the dataset."; the stimulation editor's *Compute VTA* is disabled with that reason; closing the window asks first, because quitting can leave the subject's raw data half-written: *The image import is still writing* with *Keep importing* (forget the close request), *Cancel the import, then quit* (signals the wizard's cancel, whose rollback restores the subject being written; the window then says so and only *Quit now* stays enabled) and *Quit now*. If the import ends cleanly while the question is open (every subject imported, or cancelled with a complete rollback), the application honours the close request and closes, unless unsaved lead corrections exist, which then get their own question. If it ends otherwise (a subject failed, the rollback after a cancel was incomplete, or the import stopped unexpectedly), the close request is set aside: the question closes, the application stays open with the wizard's error or result list visible, and the problem is written to the application log. A single-subject cancel whose rollback was incomplete is shown in the wizard's error window, not as the *nothing was written* notice; in a run of several subjects it is shown in the error window above the result list. DICOM conversion into the temporary folder holds back nothing: it writes nothing into the dataset.
- **Stimulation editor bound to its subject (`07` §7.1).** The editor belongs to the dataset directory and subject it was opened for; its title shows `Stimulation · sub-<id>`. The subject list stays usable while it is open. *Save* always writes the editor's subject. The editor follows the selected subject by the next rule; while it still shows another subject than the selected one (no subject selected, or the selected subject (`sub-` prefix ignored) or the dataset directory differs from the editor's), it shows the note of that rule, and for an editable label *Compute VTA* is disabled with "This editor belongs to sub-<id>, but another subject or dataset is selected" (or "…but no subject is selected") as the reason. *Compute VTA* is also disabled while a run or a run of several subjects is active (each with its own hover text) and while a wait reason stands (that reason as hover text). A compute request that still arrives is refused in a *Cannot compute the VTA* window (for another subject: naming the editor's subject and saying to select it again or to close the editor and open it for the subject wanted; otherwise: the run's refusal reason); nothing runs and the editor's waiting state and its *Computing…* status are cleared. A finished `vat` step loads VTA meshes into the editor only when the run's subject and dataset directory are the editor's; the end of a run, including one that stopped without reporting its end, clears a waiting state that was not answered.
- **Stimulation and Refine localization follow the selected subject (2026-11-12).** Once opened, each tab is for one dataset directory and subject (title `Stimulation · sub-<id>`, `Refine localization · sub-<id>`) and checks every frame whether that is still the subject highlighted in the subject list (`follow::decide`); the 3D viewer already reloads on the same change. Only the highlighted subject counts: ticking subjects for a run of several subjects moves neither tab. The rules, in this order:
  1. **Same subject**, or the tab holds nothing (never opened, closed with *Close* / *Discard*, or closed with the tab's ✖ while it showed only an empty state or was still opening a lead): nothing changes. Exception: an empty state of the selected subject is tried again once the subject's step status has been read again (a step or run finished, a review decision, *F5*, the window regaining focus), when no run, run of several subjects or job of rule 2 holds it.
  2. **A job of the tab's subject, or any run.** While a run or a run of several subjects is active (the batch moves the selection itself), or a job of the tab's own subject is running (stimulation: a *Compute VTA* waiting for its run, the VTA meshes loading, a connectivity job; Refine localization: a lead being opened or saved, review images being prepared or cleaned up, an image import writing), the tab keeps its subject and shows a warning note "<Tab> still shows sub-<a>, but sub-<b> is selected. It follows the selection when the job in progress has finished."; no buttons (a tab showing an empty state says "sub-<b> is selected. <Tab> follows the selection when the job in progress has finished." under it). The job is never moved to the other subject; the tab applies the rules again when it has ended.
  3. **Unsaved changes** (stimulation: settings that differ from the saved file, or a label created by *New…* / *Duplicate* not saved yet; a label the editor created by itself because the subject had no saved settings counts as unsaved only once it is edited, and following never writes it; Refine localization: a moved lead or an accepted side not saved): the tab keeps its subject and shows a warning note naming both subjects with three buttons: *Save and switch to sub-<b>* (writes the tab's own subject: the stimulation file at once, disabled with its reason while the settings have errors; the lead correction as *Save corrections* does, the tab switching once the save has succeeded and asking again when it failed), *Discard and switch to sub-<b>* (nothing is written) and *Stay on sub-<a>*. With no subject selected the first two read *Save and close* / *Discard and close*. *Stay* keeps the note with the two other buttons and is not asked again for that selection; after a save the note offers *Switch to sub-<b>* instead of switching by itself. After *Discard*, until the tab has switched (rule 2 or the step status still being read can delay it), the discarded settings are shown read-only with "The unsaved settings were discarded and stay read-only until the tab switches; select sub-<a> again to get them back.", and the discarded lead is not shown, with "The unsaved corrections of sub-<a> were discarded; select sub-<a> again to get them back."; selecting the tab's own subject again removes the note and undoes the unfinished *Discard* (the edits are still there). Nothing is discarded or applied to the other subject without one of these buttons; the 3D viewer does not draw the stimulation editor's VTA while it shows another subject.
  4. **No subject selected**, nothing unsaved: the tab closes its subject and says "No subject is selected." with what to do.
  5. **Otherwise** the tab opens the selected subject once the subject's step status has been read after its last change (meanwhile "<Tab> shows sub-<a>; switching to sub-<b>…", or "Switching to sub-<b>…" under an empty state; this also keeps typing in the dataset directory field from opening a subject per keystroke), without taking the focus, leaving the guided run or the start screen: the stimulation editor as *Edit stimulation…* would (its first saved label, or a new one), Refine localization as *Refine localization* would. A lead being opened this way does not hold back a run or an image import (no wait reason, no import lock); if one starts before the lead is open, the lead read before it is not shown, even when the run or import has already ended by then: while it still runs the tab says "A run or an image import started while the lead was opening. The lead opens when it has finished." and tries again by rule 1; when it has ended the lead is read again as soon as the step status has been read. A lead that arrives after the selection moved on, or after the tab was closed, is dropped and never opened in a closed tab. A selected subject that cannot be shown (no reconstruction, settings or lead that cannot be read, the step's settings unreadable) gets an empty state naming it with the reason ("Stimulation · sub-<b>: the stimulation editor cannot be shown for this subject." / "Refine localization · sub-<b>: the leads cannot be shown for this subject."), never the previous subject; the error window of the explicit buttons is not used for this.

---

## Addendum A — Implementation architecture

| Crate / module | Role |
|---|---|
| `crates/dbsloc-viz` | every viewer: 3D scene (`scene3d_gpu.rs`, `scene_cli.rs`; feature `scene-viewer`), *Refine localization* (`refine_app.rs`, `refine_lead.rs`, `refine_volume.rs`; feature `refine-embed`), registration review (`checkreg_app.rs`, `checkreg_cli.rs`; features `checkreg-embed`, `checkreg-viewer`), image preview (`volume_preview.rs`), atlas and VTA meshing, electrode meshes; byte loaders and precomputed AHEAD meshes for the browser viewer (feature `web`) |
| `crates/dbsloc-checkreg` | registration-check volume loading and the check PNG (`03` §12.2), re-exported by `dbsloc-viz` |
| `crates/dbsloc-app` | the GUI program `dbs-localizer`; a first argument `scene` or `checkreg` runs that viewer mode instead of the application |
| `crates/dbsloc-ui` | GUI state and step metadata shared by `dbsloc-app` and `dbsloc-web` (`settings.rs`: step titles such as *Write electrode image*, *Open the 3D viewer when reconstruction finishes*) |
| `crates/dbsloc-pipeline` | manifest writer (`stage_impl.rs`), atlas catalogue (`atlas_catalog.rs`, `atlas_index.rs`), backdrop list, catalogue and fetch (`backdrops.rs`, `backdrop_catalog.rs`, `backdrop_fetch.rs`), viewer launch (`display.rs`, `paths.rs`) |
| `crates/dbsloc-web` | browser viewer of the server–client plan (`docs/SERVER_CLIENT_PLAN.md` §6) |

Rules:

- **One rendering path:** `wgpu` (Vulkan, Metal, Direct3D 12) through egui / eframe paint callbacks; WGSL shaders in `crates/dbsloc-viz/src/shaders/`.
- **Adapter** (`gpu_setup.rs`): a hardware adapter that can present to the window first, the operating system's software rasteriser (WARP, lavapipe / llvmpipe) when none can, so a machine without a usable graphics driver still opens the window; `DBSLOC_FORCE_SOFTWARE_RENDERING=1` puts the software adapter first. The registration engine keeps its own adapter selection (`04` §5.4).
- **No CUDA path** exists; an optional later CUDA fast path (`AGENTS.md` §3) must produce the same outputs as the `wgpu` path. There is no Python visualization code and no VTK-class engine.
- **Off the paint thread:** manifest and volume loading, atlas and VTA meshing (rayon), backdrop loads and layer builds run on worker threads; a `wasm32` build runs them inline.
- Change this addendum only with an architecture decision (the bar of `AGENTS.md` §3).
