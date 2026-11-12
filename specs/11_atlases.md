# 11 — Atlas folders (Lead-DBS layout) and default atlas rendering

> Phase B specification written 2026-10-22 from observation of the reference implementation and its data files (no reference code). Normative for `crates/dbsloc-pipeline/src/atlas_*.rs` and the atlas path of `crates/dbsloc-viz`. Bundled copies: `resources/atlases/leaddbs/<name>/` (verbatim Lead-DBS atlas folders, GPL-3 licence beside them, see `docs/ATLAS_DISTAL.md`). Section numbers below are referenced as `specs/11` §n.

Status: Phase-B behaviour spec (derived from Lead-DBS by observation; contains no reference code).
Scope: load any Lead-DBS atlas folder (MNI152NLin2009bAsym space) and render its volumetric structures in the 3D viewer with the same default colours, opacity, visibility and side handling as Lead-DBS. Fibre-tract, discriminative-fibre and surface-heatmap entries are described only enough to recognise and skip them.

Conventions: indices written `1..N` are 1-based (as stored in the files); world coordinates are RAS millimetres of the template space; "side 1" = right hemisphere, "side 2" = left hemisphere throughout.

---

## 1. Where atlases live and which one is the default

1.1 Atlas folders are the immediate subdirectories of `templates/space/<space>/atlases/`. The atlas set name shown to the user is the folder name verbatim (e.g. `DISTAL Minimal (Ewert 2017)`). Folders whose name starts with `.` are ignored. A pseudo-entry "Use none" disables atlases.

1.2 Patient-specific (native-space) atlases may exist under `<subject>/atlases/<name>/` with the same layout; Lead-DBS lists them as `Local atlas: <name>`. Same format; out of scope for the default MNI viewer.

1.3 **Default atlas.** The default is a per-space setting stored in `templates/space/<space>/spacedef.mat`, field `spacedef.defaultatlas`. For MNI152NLin2009bAsym its value is exactly `DISTAL Minimal (Ewert 2017)` (verified by reading the file). The same file holds `defaultnucleus = "DISTAL Minimal (Ewert 2017)@STN"`. There is no `prefs.atlases.default`; the user preference `prefs.machine.defaultatlas` is copied from `spacedef.defaultatlas` when a space is selected. The library default options and the demo/BIDS readers also hard-code `DISTAL Minimal (Ewert 2017)`. Fallback when a space's `spacedef.mat` lacks the field: use the MNI152NLin2009bAsym value.

1.4 Shipped atlases (MNI152NLin2009bAsym, 72 folders, 6.8 GB in total). Selected sizes: DISTAL Nano 24 MB, DISTAL Minimal 28 MB, DISTAL Medium 66 MB, DISTAL (full) 808 MB, AHEAD 12 MB, Morel 90 MB, THOMAS 9.5 MB, Melbourne Subcortex 40 MB, Allen Brain Atlas 396 MB, Human Dysfunctome 581 MB. Full list: AHEAD Atlas (Alkemade 2020); AICHA_nuclei (Joliot 2015); Allen Brain Atlas (Ding 2020); Alzheimer Response Network (Rios 2022); ATAG_Linear / ATAG_Nonlinear (Keuken 2014); ATAG_STN (Forstmann 2012 & Keuken 2013); Atlas of the Basal Ganglia and Thalamus (He 2020); Atlas of the Human Hypothalamus (Neudorfer & Germann 2020); BGHAT (Prodoehl 2008); Brainstem Connectome (Meola 2016); Brainstem Connectome Atlas (Tang 2017); Cerebellar Functional Networks (Buckner 2011); CIT168_Reinf_Learn (Pauli 2017); Cluster Headache Atlas (Nowacki 2020); CoBrALab Merged Atlas (Chakravarty 2013); Cognitive Decline Network Atlas (Reich 2022); Cortical Functional Networks (Yeo 2011); DBS targets (Horn 2017); DBS Tractography Atlas (Middlebrooks 2020); DISTAL (Ewert 2017); DISTAL Medium / Minimal / Nano (Ewert 2017); Dystonia Response Tract Atlas (Horn 2022); Electrophysiological Atlas of GPi Activity (Neumann 2017); Electrophysiological Atlas of STN Activity (Horn 2017); Essential Tremor Hypointensity (Neudorfer 2022); Essential Tremor Lesion Network Atlas (Joutsa 2018); Essential Tremor Probabilistic Mapping (Nowacki 2022); Fornix FMRIB FA Template (Brown 2017); Functional Connectivity Atlas 7 Networks (Yeo 2011); Harvard AAN Atlas (Edlow 2012) and V2 (Edlow 2023); Human Dysfunctome Atlas (Hollunder 2024); Human Motor Thalamus (Ilinsky 2017); HybraPD Atlas (Yu 2021); Hyperdirect Pathway Atlas (Treu 2020); Levinson-Bari Limbic Brainstem Atlas (Levinson 2022); Melbourne Subcortex Atlas (Tian 2020); MNI PD25 histology / subcortical (Xiao 2017); Morel Atlas (Jakab 2008); Nigral organization atlas (Zhang 2017); Nucleus Accumbens Subregion Atlas (Cartmell 2019); OCD Response Tract Atlas (Li 2020); Oxford Thalamic Connectivity Atlas (Behrens 2003); Pallidal Dyskinesia Atlas (Tsuboi 2020); Pallidal Parcellation Atlas – Data Driven / Hypothesis Driven (Moreira da Silva 2016); PD Depression Network (Irmen 2020); PD non-motor tracts (Meyer 2025); PPN (Snijders 2016); PPN_Atlas (Alho 2017); Probabilistic Thalamus Atlas (Iglesias 2018); STN Sweetspots (Dembek 2019); STN-Subdivisions (Accolla 2014); Striatal Functional Networks (Choi 2012); Subcortical Default Mode Network Functional Atlas (Li 2021); Subcortical White Matter Pathways (Avecillas 2019); Thalamic Connectivity Atlas (Horn 2016); Thalamic DBS Connectivity atlas (Akram 2018); Thalamic Functional Atlas (Kumar 2017); Thalamic Functional Networks (Yeo 2010); THOMAS Atlas (Saranathan 2021); TOR-PSM (Elias 2020); TOR-signPD (Boutet 2021); TWH Thalamotomy Sweetspot (Boutet 2018); Ultra-high field atlas for DBS planning (Wang 2016); VIM View Atlas; Zhang Thalamic Atlas (Zhang 2008); Zona Incerta Atlas (Lau 2020).

---

## 2. Folder layout

### 2.1 Subfolders recognised by the loader

| Subfolder | Atlas type code | Meaning | Sides produced |
|---|---|---|---|
| `rh/` only (no same-named file in `lh/`) | 1 | right-hemisphere-only structure | side 1 (right) |
| `lh/` only (no same-named file in `rh/`) | 2 | left-hemisphere-only structure | side 2 (left) |
| same file name in both `lh/` and `rh/` | 3 | bilateral structure, one file per hemisphere | side 1 from `rh/`, side 2 from `lh/` |
| `mixed/` | 4 | one file containing both hemispheres; split by world x (2.4) | side 1 and side 2 |
| `midline/` | 5 | one file, one unpaired structure | single entry (stored in column 1, tag suffix `_midline`) |
| (index only) | 6 | "probabilistic, two files"; handled exactly like type 3 | sides 1 and 2 |
| `heatmap/` | 10 | NIfTI projected onto a cortical surface; not a structure mesh | out of scope |

Any other subfolder (observed: `nifti/`, `niftii/`, `fMRI_DBS_network_maps/`, ...) and any other root file (`all_histo_labels.nii.gz`, `transform_applied.txt`, `modifications.txt`, PDFs, `.m` scripts) is ignored by the atlas loader.

Root files used: `atlas_index.mat` (optional cache/metadata, §3) and `gm_mask.nii.gz` (§2.6).

### 2.2 Files inside a side folder

- Accepted: names matching `*.nii*` (both `.nii` and `.nii.gz`) → volumetric structure; `*.trk*` or `*.mat` containing a `fibers` variable → fibre tract; `.mat` containing `fibcell` → discriminative fibres. Files matching `*_temp.nii*` are scratch files and must be ignored.
- Structure name = file name with `.nii.gz` / `.nii` (or `.trk` / `.mat`) removed, e.g. `STN_motor.nii.gz` → `STN_motor`. `atlases.names` stores the full file name including extension.
- Both NIfTI-1 compressed and uncompressed are valid; the observed files are float32 (datatype 16), sform_code = qform_code = 2, axis-aligned affines (positive or negative diagonal, e.g. AHEAD has y step −0.5 mm).

### 2.3 Structure order when no `atlas_index.mat` exists

Concatenate, in this order: right-only files, left-only files, bilateral files (in the order they appear in the `lh/` listing), mixed files, midline files, heatmap files. Each listing is the file-system listing sorted by name (Lead-DBS relies on its directory listing order; byte-wise lexicographic order is the expected result, case handling unverified). When `atlas_index.mat` exists its `names` order is authoritative (it is frequently hand-reordered; DISTAL Minimal starts with `STN`, not alphabetically).

### 2.4 Splitting `mixed/` files

A mixed file is split purely geometrically, never by label value. For every voxel compute its world x (centre, via the image affine). Right part = copy of the image with all voxels of x < 0 set to 0. Left part = copy with all voxels of x > 0 set to 0. Voxels with x exactly 0 are kept in both. Each part is then treated like a separate single-side file (crop, threshold, mesh), with the same structure name; tags get `_right` / `_left`.

### 2.5 Mask value conventions (binary vs probabilistic)

For each loaded image: zeros, NaN and ±Inf are "outside". A structure is **binary** when the set of distinct inside values has exactly one element (DISTAL sub-zones store the single value 0.99931…, not 1.0). Otherwise it is **probabilistic/graded**; its inside values are shifted down by the smallest inside value (so the smallest inside voxel becomes 0, i.e. outside) before thresholding. Thresholding and meshing then operate on this processed image.

Integer label images (one NIfTI with many labels plus a `.txt` "index name" list) are **not** an atlas-folder format. Such files exist in `templates/space/<space>/labeling/` (parcellations for connectivity analysis, `.txt` lines like `1 G_Frontal_Sup-1-L`); the 3D atlas loader never reads them, and a stray root-level label file (e.g. `all_histo_labels.nii.gz` in DISTAL full) is ignored.

### 2.6 `gm_mask.nii.gz`

A grey-matter mask derived from the atlas at index-build time: a 0.3 mm grid accumulates, per voxel, how many thresholded structure files (`tissuetypes` = 1 only) cover it, then is binarised at > 1.5, cropped, and smoothed with a 1 mm FWHM Gaussian. Because the bilateral (type 3) and mixed (type 4) files are each added twice by the build, in practice it is the union of those structures, while single-side and midline structures usually drop out (literal, probably unintended behaviour). Example: DISTAL Minimal 201×123×87 voxels at 0.3 mm, float32 in [0, 1]. It is used only by VTA / lead-field modelling (`prefs.vat.gm = 'mask'`), never by the 3D rendering. Wyss's viewer may ignore it; it may be absent for fibre-only atlases.

---

## 3. `atlas_index.mat`

### 3.1 Container format

All 72 shipped files are **MATLAB v7.3 = HDF5** (text header "MATLAB 7.3 MAT-file …" in a 512-byte user block; HDF5 superblock at offset 512). Reading rules for an HDF5 reader:

- One top-level group `atlases` (attribute `MATLAB_class = "struct"`); the reserved group `#refs#` holds referenced objects.
- Arrays are column-major: an HDF5 dataset of shape (a, b) is a MATLAB array of shape (b, a). Transpose when reading.
- `MATLAB_class = "char"`: uint16 code units, read in column-major order into a string.
- `MATLAB_class = "cell"`: dataset of HDF5 object references, one per cell element (column-major).
- Struct arrays (e.g. `presets`): group whose field datasets are reference arrays, one reference per struct element.
- Empty arrays: a small uint64 dataset holding the dimensions with attribute `MATLAB_empty = 1`.
- `roi` elements are saved as plain structs (group, `MATLAB_class = "struct"`), not as MATLAB objects.

Older v7 (non-HDF5) files may exist in third-party atlases; support is optional (none shipped).

### 3.2 Fields of `atlases`

Notation: N = number of structures. "Cell N×2" means MATLAB shape; column 1 = right / midline, column 2 = left.

| Field | Type / shape | Present in (of 72) | Semantics |
|---|---|---|---|
| `names` | cell N×1 of char | 72 | File names with extension; defines structure order and indices 1..N |
| `types` | double 1×N (or N×1) | 72 | Atlas type code per structure (§2.1) |
| `colormap` | double K×3 in [0, 1] | 72 (all numeric) | Palette; K need not equal N (e.g. 36, 64, N+1). Lead-DBS also accepts a colormap *name* string here (not observed) |
| `colors` | double N×1 | 72 | 1-based index into `colormap` per structure; may be non-integer (Morel: fractional, max 64) or NaN (discriminative fibres) |
| `threshold` | struct {`type`: char, `value`: double scalar or vector} | 72 | Threshold rule (§4.2); default `relative_intensity`, 0.5 |
| `roi` | cell N'×2 (or N'×1 for midline-only atlases) of structs | 70 | Cached per-side structure objects incl. the voxel image, colour, alpha, threshold and meshes (§3.3). N' ≤ N: trailing non-volumetric entries may have no column; non-volumetric cells may be empty |
| `fv` | cell N×2 | 72 | Cached meshes for fibre/surface entries; empty for volumetric structures |
| `XYZ` | cell N×2 of struct {`vx` M×3 (1-based voxel indices), `mm` M×3 (world mm), `val` M×1, `dims` 1×3 voxel size mm} | 72 | Inside voxels (value > 0 after §2.5 processing) for contact statistics; not used for rendering |
| `pixdim` | cell N×2 | 72 | 3×1 voxel size (mm) for volumetric entries; or the strings `fibers`, `surface`, `discfibers` — the canonical discriminator of entry kind |
| `tissuetypes` | double 1×N | 72 | 1 = grey matter (enters `gm_mask`); 2 = treated as white matter / fibre in some code paths (exact meaning outside rendering uncertain) |
| `version` | double scalar | 72 | Index format version; 2.1 in all shipped files. < 2.1 forces a rebuild |
| `rebuild` | double scalar | 72 | 1 forces regeneration from the NIfTI files; 0 in all shipped files |
| `citation` | struct {`name` char, `short` char, `long` cell of char} | 62 | Display name and references (§6) |
| `defaultset` | double scalar | 33 | 1-based index into `presets` selecting the initially shown subset; absent → show all |
| `presets` | struct array {`label` char, `show` int list, `hide` int list, `default` char} | 19 | Named visibility presets (§5.2) |
| `labels` | cell L×1, each a cell 1×N of char | 9 | Alternative display names per structure, aligned with `names` |
| `labelnames` | cell L×1 of char | 9 | Names of the label sets (DISTAL: `Abbreviations`, `Names`) |
| `subgroups` | struct array {`label`, `entries`} | 1 | Grouping of structures in the atlas control tree |
| `heatcolormap` | double K×3 | 9 | Colormap for type-10 surface heatmaps |
| `discfibersonly` | double scalar | 23 | Cached flag; 1 when every entry is discriminative fibres |
| `preset2d` | int list | 2 | Structures shown in 2D slice exports |

The cache is considered stale (Lead-DBS regenerates it from the folder and overwrites the file) when the file has neither `roi` nor `fv`, has no `version` or `version < 2.1`, or `rebuild == 1`. Wyss must never write into atlas folders; regenerate in memory instead.

### 3.3 Fields of each `roi{i, side}` struct

| Field | Type | Meaning / observed default |
|---|---|---|
| `name` | char | structure name without extension (e.g. `STN`) |
| `Tag` | char or empty | display tag; when empty the viewer uses `<name>_right` / `_left` / `_midline` |
| `niftiFilename` | char | absolute path on the machine that built the index (stale, ignore) |
| `nii` | struct | cached image: `img` (3-D double, the cropped voxel data, identical to the on-disk file in the files checked), `mat` (4×4 voxel→mm for **1-based** indices; equals the NIfTI sform with translation `t − A·[1,1,1]`), `dim`, `voxsize`, header copy |
| `threshold` | double | iso-level used for the surface, in processed-image units (§2.5) |
| `color` | double 1×3 in [0, 1] | face colour actually rendered. In two atlases it is the string `none` (with `usesolidcolor = 0`) |
| `alpha` | double | face opacity; 0.7 almost everywhere (exceptions: VIM View Atlas 0.5/0.1, ET Probabilistic Mapping 0.3, Cognitive Decline 0.2) |
| `usesolidcolor` | 0/1 | 1 = single colour; 0 = per-vertex colour from image intensity via `colormap` field of the roi |
| `binary` | 0/1 | §2.5 classification |
| `smooth` | double | smoothing iterations stored (5; Dembek sweetspots 0; TOR-signPD 43). **Overridden at display time by the preference `hullsmooth` = 5** |
| `hullsimplify` | double or char | face-reduction parameter (0.3 typical; 0.5; 1 = none; char `auto`) — used as stored |
| `max`, `min` | double | cached intensity extrema (may be stale; DISTAL Minimal STN stores 0.9 while the image max is 1.0) — informational only |
| `fv` | struct {`vertices` V×3 mm, `faces` F×3 1-based} | unsmoothed iso-surface + caps |
| `sfv` | struct {`vertices`, `faces`} | smoothed and reduced mesh — what is displayed |
| `cdat` | 3×V | jittered per-vertex colour (unused when `usesolidcolor = 1`) |
| `edgecolor` | char | `none` |
| `Visible` | char | `on` / `off` (stored state; the viewer recomputes from presets) |
| `AmbientStrength`, `DiffuseStrength`, `SpecularStrength`, `SpecularExponent`, `SpecularColorReflectance` | double | 0.3, 0.4, 0.3, 3, 1 |
| `colormap` | K×3 or empty | per-vertex colormap for non-solid rendering |

**Side mapping verified:** in DISTAL Minimal every `roi{i,1}` mesh lies at x > 0 (right) and every `roi{i,2}` at x < 0 (left).

---

## 4. Turning a structure into a surface

### 4.1 Pipeline (per structure, per side)

1. Load the voxel image: prefer `roi{i,side}.nii.img` + `.nii.mat` from the index; otherwise the NIfTI file from the side folder (for `mixed`, the split part, §2.4). Both give the same data for shipped atlases.
2. When building without an index, Lead-DBS additionally (a) crops the image to the bounding box of non-zero voxels plus a rim of round(2 mm / mean voxel size) voxels, and (b) if any voxel dimension exceeds 0.8 mm, reslices it to 0.4 mm isotropic with nearest-neighbour interpolation. Cropping does not change the surface; the 0.4 mm reslice does (blockier steps follow the coarse voxels). Wyss: crop in memory; the reslice is optional and should be documented as a deviation if omitted.
3. Classify and process values (§2.5).
4. Threshold: use `roi{i,side}.threshold` when present; otherwise compute it from the **unprocessed** image with the rule in §4.2.
5. Extract the iso-surface at the threshold (marching-cubes class algorithm, linear interpolation along edges), and add cap faces wherever the above-threshold region touches the volume boundary so the surface is closed.
6. Map vertices to world mm: x_mm = A · [i, j, k, 1]ᵀ with the NIfTI affine and **0-based** voxel indices (equivalently the 1-based `mat` with 1-based indices). Lead-DBS builds per-axis coordinate grids, which is exact only for axis-aligned affines; Wyss should apply the full affine (identical result for every shipped atlas).
7. Smooth: if the smoothing value s (preference `hullsmooth`, default **5**) is > 0, run s iterations of curvature-flow smoothing (§4.3) with λ = 1. s = 0 → no smoothing.
8. Reduce: with r = `hullsimplify` (stored; preference default 0.3): 0 < r < 1 → decimate to fraction r of the faces (shipped meshes: exactly 0.30 of the unsmoothed face count); r > 1 → target r faces in total (fraction r / unsmoothed face count); r == 1 → no reduction; text `auto` → target 700 faces. Any quality edge-collapse decimation is acceptable (Lead-DBS uses MATLAB's built-in reducer; algorithm not specified).
9. If the mesh is empty at the threshold, Lead-DBS halves the threshold and retries (up to 200 times) to find a centroid for the label; an image with no non-zero voxel produces no surface.

### 4.2 Threshold rules (`atlases.threshold.type`)

Let I = the image, P = sorted ascending list of its values > 0, n = |P|, v = `threshold.value` (scalar or per-structure vector, indexed by the 1-based structure index).

| Type | Threshold |
|---|---|
| `relative_intensity` (default, v = 0.5) | max(I) · (1 − v) |
| `relative_intensity_vector` | max(I) · (1 − v[i]) |
| `absolute_intensity` | v |
| `absolute_intensity_vector` | v[i] |
| `percentage` | P[round(n · (1 − v))] (keeps fraction v of voxels; falls back to P[1] on an out-of-range index) |
| `percentage_vector` | as `percentage` with v[i]; if v[i] == 1 the threshold is P[n] minus machine epsilon (literal behaviour) |
| unknown type / no field | max(I) · 0.5, with a warning |

Observed: 67 atlases use `relative_intensity` 0.5; Cluster Headache `absolute_intensity_vector`; STN Sweetspots `absolute_intensity` 0.001; ET Lesion Network `percentage_vector`. Note the stored per-roi threshold can differ from the rule: DISTAL STN, GPi, RN and GPe store **0.4** although the rule gives 0.5 (image max 1.0). Use the stored value for parity.

### 4.3 Curvature-flow smoothing (Desbrun et al. 1999 family; original implementation BSD-licensed, D.-J. Kroon 2009)

Per iteration, for every vertex i with one-ring neighbours j₁…j_m in cyclic order (Jacobi update: all positions read from the previous iteration):

- For neighbour j_k, take its ring predecessor p = j_{k−1} and successor q = j_{k+1} (cyclic). Let α be the interior angle at p in triangle (i, p, j_k) and β the interior angle at q in triangle (i, q, j_k). Weight w_k = (π − α) + (π − β). Directions are normalised with +1e-14 added to lengths.
- U = Σ w_k (x_i − x_{j_k}); W = 1e-14 + Σ w_k.
- x_i ← x_i − λ · U / W, λ = 1.

Exact parity is not required; a cotangent or umbrella Laplacian with comparable shrinkage is an acceptable documented deviation.

### 4.4 Material and opacity

Each structure-side is one patch: face colour = `roi.color` (solid), face opacity = `roi.alpha` (default **0.7**), no edges, Gouraud shading for faces and edges, ambient 0.3, diffuse 0.4, specular 0.3, specular exponent 3, specular colour reflectance 1 (highlight takes the light colour). Scene lights: an infinite head-light coloured (0.8, 0.8, 1.0); a local light at (0, 0, 10) mm coloured (1.0, 0.9, 0.9); infinite lights from (−100, 0, 0) coloured (1.0, 0.9, 0.7) and from (+100, 0, 0) coloured (0.9, 0.9, 1.0).

For `usesolidcolor = 0` and non-binary images Lead-DBS colours each vertex by the image value sampled at it, contrast-stretched and mapped through the roi `colormap` (default: 64-step blue → white → red). Optional; rendering such a structure solid in its palette colour is an acceptable v1 deviation.

---

## 5. Colours, visibility and grouping

### 5.1 Colour resolution (per structure i, side s)

1. If `roi{i,s}.color` is a numeric 1×3 → use it. This is what Lead-DBS renders. In 68 of the 70 atlases with a `roi` it equals `colormap[round(colors[i])]` for every structure; in TOR-PSM (18 structure-sides) and Essential Tremor Lesion Network Atlas (one structure with colour `none`, one with a colour absent from its palette) it differs, so the roi value wins. A roi colour of `none` (text) means "no solid colour"; fall back to rule 2.
2. Else, if `colormap` is numeric and `colors[i]` is finite → `colormap[round(colors[i])]` (clamp the index to 1..K; an index rounding to 0 becomes 1).
3. Else (no `atlas_index.mat`): **default palette** = the 14-stop "BuOrR_14" palette (othercolor collection, BSD) resampled to N entries with shape-preserving piecewise-cubic Hermite (PCHIP, Fritsch–Carlson, MATLAB/SciPy end conditions) at N equally spaced positions in [0, 1] (for N = 1 the single position is 1.0), each channel clipped to [0, 1]. Structure i gets entry i (index formula i · K / N with K = N). Both sides share the colour. BuOrR_14 stops, in order:

(0.03, 0.353, 1), (0.2, 0.467, 1), (0.35, 0.567, 1), (0.55, 0.7, 1), (0.75, 0.833, 1), (0.9, 0.933, 1), (0.97, 0.98, 1), (1, 1, 0.8), (1, 1, 0.6), (1, 1, 0), (1, 0.8, 0), (1, 0.6, 0), (1, 0.4, 0), (1, 0, 0).

Check values (computed with SciPy PCHIP, tolerance 1e-3): N = 4 → (0.03, 0.353, 1.0), (0.8072, 0.8711, 1.0), (1.0, 1.0, 0.1333), (1.0, 0.0, 0.0); N = 15 → entry 1 (0.03, 0.353, 1.0), entry 8 (0.9902, 0.9935, 0.925), entry 15 (1.0, 0.0, 0.0).

The colour swatches in the atlas control use `colormap[ceil(colors[i])]` (not the roi colour) scaled by 256 and rounded — cosmetic only.

### 5.2 Default visibility and presets

- With `presets` and `defaultset`: a structure is initially visible iff its index is in `presets[defaultset].show`; all its sides follow. Without `presets`: all visible. (`hide` and `default` are not consulted at start-up.)
- Applying a preset later: indices in `show` → visible; in `hide` → hidden; others: `default = "absolute"` → hidden, `"relative"` → unchanged; any other text (AHEAD uses `mask structures`) behaves as unchanged.
- The preset menu lists two built-ins first — "Show all structures" (show all, absolute) and "Hide all structures" (hide all, absolute) — then the atlas presets; the initially selected entry is `defaultset + 2` when atlas presets exist.

### 5.3 Atlas control and labels

- Tree: one node per subgroup (default: a single "Structures" node containing every non-discriminative-fibre entry), one checkbox child per structure (label = name without extension, or `labels{k}[i]` when the user selects a label set; the menu lists `labelnames` plus "NIfTI filenames"), and under it one checkbox per side: `RH` and `LH` for types 3/4/6/10, `RH` for type 1, `LH` for type 2, `Midline` for type 5. Toggling a structure toggles all its sides.
- Toolbar: one toggle button per structure-side, coloured icon, tooltip = tag (`STN_right`).
- 3D text labels: one per structure-side at the mean of its displayed mesh vertices, text = tag with underscores replaced by spaces (`STN right`), white, bold, 12 pt, **hidden by default**.

### 5.4 DISTAL Minimal (default atlas) — reference table

15 structures, all type 3 (files in both `lh/` and `rh/`, 0.22 mm voxels, float32), `threshold` = relative_intensity 0.5, every roi alpha 0.7, smooth 5, hullsimplify 0.3, solid colour. `defaultset` = 1 → initially visible: 1 STN, 5 GPi, 14 RN, 15 GPe.

| # | File | Abbreviation / Name (`labels`) | `colors` | RGB (float) | RGB ×255 | Stored threshold (R / L) | Binary |
|---|---|---|---|---|---|---|---|
| 1 | STN.nii.gz | STN / Subthalamic Nucleus | 1 | 1.0, 0.5, 0.0 | 255, 128, 0 | 0.4 / 0.4 | no |
| 2 | STN_motor.nii.gz | STN sensorimotor / Subthalamic nucleus (sensorimotor) | 2 | 1.0, 0.5, 0.0 | 255, 128, 0 | 0.49966 / 0.49950 | yes |
| 3 | STN_associative.nii.gz | STN associative | 3 | 0.0, 0.75, 1.0 | 0, 191, 255 | 0.49966 / 0.49950 | yes |
| 4 | STN_limbic.nii.gz | STN limbic | 4 | 0.9763, 0.9831, 0.0538 | 249, 251, 14 | 0.49966 / 0.49950 | yes |
| 5 | GPi.nii.gz | GPi / Internal Pallidum | 5 | 0.5, 1.0, 0.5 | 128, 255, 128 | 0.4 / 0.4 | no |
| 6 | GPi_sensory.nii.gz | GPi sensory | 9 | 0.8, 0.2, 0.7 | 204, 51, 179* | 0.50005 / 0.50002 | yes |
| 7 | GPi_primarymotor.nii.gz | GPi primarymotor | 7 | 1.0, 0.2, 0.2 | 255, 51, 51 | 0.50005 / 0.50002 | yes |
| 8 | GPi_premotor.nii.gz | GPi premotor | 8 | 0.8, 0.7, 0.4 | 204, 179*, 102 | 0.50005 / 0.50002 | yes |
| 9 | GPi_sensorimotor.nii.gz | GPi sensorimotor | 6 | 1.0, 0.5, 0.0 | 255, 128, 0 | 0.50005 / 0.50002 | yes |
| 10 | GPi_postparietal.nii.gz | GPi postparietal | 11 | 0.2, 0.5, 1.0 | 51, 128, 255 | 0.50005 / 0.50002 | yes |
| 11 | GPi_occipital.nii.gz | GPi occipital | 12 | 0.4, 0.4, 0.6 | 102, 102, 153 | 0.50005 / 0.50002 | yes |
| 12 | GPi_temporal.nii.gz | GPi temporal | 13 | 0.6, 0.4, 0.4 | 153, 102, 102 | 0.50005 / 0.50002 | yes |
| 13 | GPi_prefrontal.nii.gz | GPi prefrontal | 10 | 0.1, 0.2, 1.0 | 26, 51, 255 | 0.50005 / 0.50002 | yes |
| 14 | RN.nii.gz | RN / Red nucleus | 15 | 0.5, 0.0, 0.0 | 128, 0, 0 | 0.4 / 0.4 | no |
| 15 | GPe.nii.gz | GPe / External Pallidum | 14 | 0.0, 0.5, 1.0 | 0, 128, 255 | 0.4 / 0.4 | no |

\*×255 values use round-half-up (0.7·255 = 178.5 → 179; Python's banker's rounding gives 178). Compare float colours with tolerance 1e-4.

Presets: 1 "Primary DBS targets" (absolute, show 1 5 14 15); 2 "STN: Solid" (relative, show 1, hide 2–4); 3 "STN: Subzones" (show 2–4, hide 1); 4 "GPi: Solid" (show 5, hide 6–13); 5 "GPi: Sensorimotor functional zone" (show 5 9, hide 6–8 10–13); 6 "GPi: Detailed functional zones" (show 6–8 10–13, hide 5 9).

DISTAL Medium: the same 15 entries (entries 6 and 9 swap files: 6 = GPi_sensorimotor, 9 = GPi_sensory, each keeping its colour above) plus 16 Thal (1.0, 1.0, 0.42353), 17 VA (0.42050, 0.48150, 0.43139), 18 VIM (0.40522, 0.46189, 0.53161), 19 VoiCm (0.51417, 0.46622, 0.46622); preset 1 additionally shows 16–19. DISTAL Nano: GPe (0, 0.5, 1), GPi (0.5, 1, 0.5), RN (0.5, 0, 0), STN (1, 0.5, 0).

Cached displayed meshes (DISTAL Minimal, `sfv`; useful as a coarse oracle): STN right 1779 vertices / 3554 faces, centroid (11.49, −12.61, −7.62) mm, bbox x 6.7…15.8, y −17.9…−8.2, z −11.4…−4.1; STN left centroid (−11.49, −13.33, −7.70); GPi right centroid (17.62, −4.71, −3.89); RN right (5.17, −18.78, −8.92); GPe right (19.99, −3.52, −0.75). Reduced face count = 0.30 × unsmoothed (STN right 11852 → 3554).

---

## 6. Licensing and citations

- Lead-DBS `LICENSE.md`: the software is GPL-3. A section "The Standard Space Atlases" states that the atlases in the repository are released under the main Lead-DBS licence (GPL-3) at their owners' request (Hammersmith atlas excepted, with its own academic non-commercial licence), that they **should not be used for commercial purposes**, and that commercial users should contact the primary co-ordinator of the relevant atlas.
- No atlas folder carries its own licence or readme file. Only `transform_applied.txt` (9 folders), `modifications.txt` (Fornix), PDFs (PPN Snijders) and a build script (TOR-PSM) exist. Citation data lives only in `atlas_index.mat` → `citation` (62 of 72).
- DISTAL Minimal citation: name "DISTAL Minimal Atlas (Ewert 2017)", short "Ewert et al. 2017", long = Ewert S, Plettig P, Li N, Chakravarty MM, Collins DL, Herrington TM, et al. (2017) *Toward defining deep brain stimulation targets in MNI space: A subcortical atlas based on multimodal MRI, histology and structural connectivity*, NeuroImage; and Chakravarty MM, Bertrand G, Hodge CP, Sadikot AF, Collins DL (2006) *The creation of a brain atlas for image guided neurosurgery using serial histological data*, NeuroImage 30(2):359–376. No separate DISTAL licence statement was found in the Lead-DBS tree.
- Consequence for Wyss DBS Localizer (GPL-3.0-only since 2026-11-10; this paragraph was written when it was proprietary): only the default atlas folder is bundled, with the Lead-DBS licence beside it (`docs/ATLAS_DISTAL.md`); all other folders are user-supplied. Show `citation.name` / `citation.long` in the viewer when present. The colour constants in §5.4 are factual display parameters; legal review is advised before hard-coding them as product defaults.

---

## 7. Uncertainties

- The exact triangulation of MATLAB's iso-surface and its decimation algorithm are not specified; vertex/face counts will differ (target: same counts within ±20 %, same centroid within 0.2 mm).
- Directory listing order on case-mixed names (only relevant without an index).
- Contrast-stretch details of non-solid per-vertex colouring (§4.4).
- `tissuetypes` meanings other than 1.
- Type 6 never observed in a folder; treated as type 3.

---

## 8. Acceptance checks

1. The default atlas set for MNI152NLin2009bAsym resolves to `DISTAL Minimal (Ewert 2017)` (from `spacedef.mat` `defaultatlas`, or a constant when that file is unavailable).
2. The v7.3 `atlas_index.mat` of DISTAL Minimal loads; `names` has 15 entries in the §5.4 order; all `types` are 3; `roi` is 15×2.
3. 30 structure-side meshes are built; every side-1 mesh has centroid x > 0 and every side-2 mesh x < 0.
4. Rendered colours (float, tolerance 1e-4): STN (1.0, 0.5, 0.0); GPi (0.5, 1.0, 0.5); RN (0.5, 0.0, 0.0); GPe (0.0, 0.5, 1.0); STN_associative (0.0, 0.75, 1.0); STN_limbic (0.9763, 0.9831, 0.0538); GPi_sensory (0.8, 0.2, 0.7); GPi_prefrontal (0.1, 0.2, 1.0); both sides identical.
5. Opacity 0.7 for every DISTAL Minimal structure; no edges drawn.
6. Initially visible: STN, GPi, RN, GPe on both sides (8 meshes); the other 11 structures (22 meshes) hidden; 3D text labels hidden.
7. Applying preset 3 "STN: Subzones" shows STN_motor, STN_associative and STN_limbic, hides STN, leaves GPi/RN/GPe as they were.
8. STN right is meshed at iso-level 0.4 (stored), not 0.5; the enclosed voxel count is 16215 (voxels > 0.4 in `rh/STN.nii.gz`). Mesh centroid within 0.2 mm of (11.49, −12.61, −7.62); bounding box within 0.3 mm of the §5.4 values.
9. Vertices are in world mm through the NIfTI affine with 0-based indices: the unsmoothed STN right iso-surface at 0.4 (before §4.1 step 7) has bounding box within 0.05 mm of min (6.673, −17.988, −11.503), max (15.871, −8.139, −3.972) (the cached `roi{1,1}.fv`); a one-voxel index error shifts it by 0.22 mm.
10. Without `atlas_index.mat` (copy of DISTAL Nano minus the index), the 4 structures load in order GPe, GPi, RN, STN with default colours (0.03, 0.353, 1.0), (0.8072, 0.8711, 1.0), (1.0, 1.0, 0.1333), (1.0, 0.0, 0.0) (tolerance 1e-3), all visible, threshold = 0.5 × image maximum, and nothing is written into the atlas folder.
11. A `mixed/` file is split at world x = 0: right part has no voxel with x < 0, left part none with x > 0; tags `<name>_right` / `<name>_left`. Check: CIT168_Reinf_Learn `mixed/Ca.nii.gz` yields two meshes.
12. A `midline/` structure (e.g. Fornix FMRIB FA Template) yields one mesh tagged `_midline` and one `Midline` checkbox.
13. TOR-PSM structure 2 renders with its roi colour (0.78, 0.808, 0.965) (3 decimals), not `colormap[colors[2]]`.
14. Entries whose `pixdim` is `fibers`, `surface` or `discfibers` (e.g. DBS Tractography Atlas, PD Depression Network heatmap) are skipped with a log note and do not prevent the volumetric entries of the same atlas from rendering.
15. The atlas citation (`citation.name`, `citation.short`) is available to the viewer for DISTAL Minimal ("DISTAL Minimal Atlas (Ewert 2017)", "Ewert et al. 2017").

## 9. Implementation notes (2026-10-22, `crates/dbsloc-viz/src/atlas_sets.rs`, `atlas_marching.rs`, `scene3d_gpu.rs`)

**Decisions on unspecified points.** When one structure index appears in both `show` and `hide` of a preset, `hide` wins. A global `atlas_default_on_substrings` applies to flat-layout folders (AHEAD) only; a Lead-layout folder without an index shows every structure unless substrings are given explicitly. When an index side has no colour the BuOrR palette of §5.1 is used. A structure that is empty at its threshold gives an empty mesh (logged), not an error.

**Deviations from §4 (accepted for now).** No decimation (§4.1 step 8: the STN right has about 5.3 k vertices against 1.8 k in the reference); smoothing is a pre-blur (six passes of a 3 × 3 × 3 box) followed by ten Taubin passes instead of curvature flow (§4.3); no 0.4 mm reslice for voxels coarser than 0.8 mm (§4.1 step 2b); the halve-and-retry threshold search (§4.1 step 9) is not done; rows are one per mesh (structure and side), not a structure tree with side children; per-vertex colouring of non-solid structures (§4.4) is not done. The marching cubes draw the surface at the threshold of §4.2 on the graded values (§2.5), on a crop padded past the volume with zeros so structures touching the boundary close. Measured on the STN right of DISTAL Minimal: 16 215 voxels above 0.4 (§8.8), raw iso-surface bounding box within 0.05 mm of §8.9, smoothed centroid within 0.12 mm and bounding box within 0.25 mm of the reference, 78 ms per structure in a release build.

**Scene manifest.** `atlas_lead_sets[].index` carries the index (`AtlasIndexV1`: version, citation, structures with 0-based `index`, `file`, `name`, `kind`, `tissue_type`, sides with `side`, `tag`, `color`, `alpha`, `threshold`; presets with `show`, `hide`, `mode`; `default_preset`; label sets; skipped entries), optional fields default; integer fields accept integer-valued floats. The viewer resolves `atlas_id` (persisted by the GUI, `dbs-localizer scene --atlas <id>`, unknown ids fall back to the default set) and shows the set combo with the citation, a preset combo (*Show all*, *Hide all*, then the index presets) and a *Structure names* combo for label sets. The per-user mesh cache key is version 6 and includes the threshold and hemisphere split.

---

## 10. Downloading Lead-DBS atlases (amendment 2026-11-11; normative)

**Why.** Every atlas beyond the bundled DISTAL Minimal used to need a Lead-DBS installation (`DBSLOC_LEADDBS_ATLASES_DIR`). The Lead-DBS maintainers stated on 2026-11-11 that the data packages they host carry no licence and that their use by other software is not explicitly forbidden (`specs/15` §2.4); the owner reads this as covering downloads by Wyss DBS Localizer. Wyss DBS Localizer therefore lets the user download single atlases from the Lead-DBS host onto their own computer. Wyss DBS Localizer never mirrors or redistributes them.

### 10.1 Source

- The atlases exist only inside the Lead-DBS data package `leaddbs_data.zip` (8 473 093 428 bytes on 2026-11-11; deflate only, no encryption; 2 322 entries, 73 atlas folders under `templates/space/MNI152NLin2009bAsym/atlases/`, 0.4 MB to 812 MB compressed each). The Lead-DBS git repository ignores `atlases/`.
- Address: `https://uni-koeln.sciebo.de/public.php/dav/files/8s7cxAWA4PA5GZq/leaddbs_data.zip`, the public WebDAV share that `download.php` redirects to (`specs/15` §2.1). The host answers `Range` requests with 206 and publishes the whole file's SHA-1 in the `OC-Checksum` header (`SHA1:<hex>`).
- Only the atlas folders are taken. Templates and every other entry of the package are never downloaded.

### 10.2 Pinned manifest

- `resources/atlases/leaddbs_download.json`, written by `scripts/pin_leaddbs_atlases.py` (maintainer tool, standard library only): it streams the package once, checks every atlas member's size and CRC-32 (the other members are covered by the whole-stream SHA-1), checks the whole stream against the host's SHA-1, applies the path and name rules of this section and §10.9, and writes nothing when any check fails. It is embedded in the pipeline (`include_str!`); there is no network catalogue.
- Shape: `package` = `{file_name, url, bytes, sha1, last_modified, central_directory_offset, atlas_prefix, pinned}`; `atlases` = list, sorted by name (byte order), of `{name, bytes, files: [{path, bytes, sha256}]}`, `path` relative to the atlas folder with `/` separators, `bytes` uncompressed, sorted by path.
- A pinned `path` must be relative, without empty, `.` or `..` components, backslash or `:`; the pipeline refuses to load a manifest that breaks this (test on the embedded file).
- A maintainer re-runs the script when Lead-DBS replaces the package and commits the new manifest.

### 10.3 Download of one atlas (`crates/dbsloc-pipeline/src/atlas_fetch.rs`)

1. **Choose.** The atlas is named exactly as in the manifest. Unknown name → error with the list command as hint. A name equal to a bundled atlas folder (`resources/atlases/leaddbs/<name>`) is refused ("already bundled").
2. **Package check.** `HEAD` the URL. A size or `OC-Checksum` different from the pin is not an error by itself: it is logged ("the Lead-DBS data package changed on <Last-Modified>; files are checked one by one"), because per-file pins decide (step 5).
3. **Directory.** Read the end-of-central-directory record (ZIP64 when present) and the central directory with `Range` requests. No 206 → error `the host does not support partial downloads`. Take the entries whose name is `<atlas_prefix><name>/<path>` (directory entries ignored). The set of `path`s must equal the manifest's set for that atlas, else error `the Lead-DBS atlas package changed; this atlas needs an update of Wyss DBS Localizer` and nothing is written.
4. **Fetch.** Sort the atlas's entries by local-header offset and group them into runs of adjacent entries (an entry ends where the next entry of the whole central directory begins, or at the central directory). Each run is one `Range` request, read as a stream: local header (signature checked, its own name and extra lengths used), then the compressed data. On an interrupted read the request is repeated from the current byte with `If-Range: <ETag>` (at most 5 attempts per run, waits 5 s, 10 s, 20 s, 40 s, 60 s); a changed ETag aborts. Method 0 (stored) and 8 (deflate) only; encrypted entries are refused.
5. **Verify.** For each file: uncompressed size, CRC-32 from the central directory and SHA-256 from the manifest must match. The first mismatch aborts the atlas; the staging folder is deleted.
6. **Install.** Files are written to `<user data dir>/atlases/.staging-<name>-<pid>/` using the manifest path (never the zip name), then the folder is renamed to `<user data dir>/atlases/leaddbs/<name>/` (`leaddbs_data::user_atlases_dir`, the same root the Lead-DBS import copies into, `specs/17`). An existing folder of that name is replaced only after the new one is complete (rename old aside, rename new in, delete old). Leftover staging folders older than one day are deleted at the next download.
7. **Record.** `<atlas folder>/.dbsloc-install.json`: `{atlas, source: "leaddbs_data.zip", package_sha1 (pinned), package_last_modified (from HEAD), files, bytes, installed (UTC ISO date), version: 1}`.

Rules: one atlas download at a time per user data folder (lock file `<user data dir>/atlases/.download.lock`, same stale-lock rule as `specs/15` §4.4); product `User-Agent`; no parallel requests; free space checked before start (atlas `bytes` + 10 % + 100 MB, else error with the numbers); a cancel flag is checked between chunks and leaves no partial atlas. `DBSLOC_ATLAS_DOWNLOAD_URL` replaces the package URL (tests and mirrors under the user's control); the manifest's pins still apply.

### 10.4 Notice shown before every download (CLI and GUI, verbatim)

> Lead-DBS atlases are downloaded from the Lead-DBS server to this computer. Lead-DBS states that its standard-space atlases are released under the GNU GPL v3 at their owners' request and should not be used for commercial purposes; for commercial use, contact the atlas's primary coordinator. Cite the atlas publication shown with the atlas.

No acceptance box: the statement is a request, and no data use agreement applies.

### 10.5 Where downloaded atlases appear

Atlas discovery (§1, `atlas_catalog.rs`) lists, in this order: bundled folders (`leaddbs:<folder>`), downloaded folders in `<user data dir>/atlases/leaddbs/` (`user:<folder>`), the external Lead-DBS directory (`external:<folder>`), then AHEAD. The user-data root is skipped when its canonical path equals the external root (the Lead-DBS import's copy mode points the external setting there; those folders keep their `external:` ids). There is no dedupe by folder name: ids are persisted in group records, statistics settings and the GUI, so a set keeps its id when the same name appears in another root. Staging folders (`.`-prefixed) are ignored as before. Every caller that writes a scene manifest or resolves atlas names (reconstruct, visualize, group view, group calculate, server) gets the downloaded atlases without a new setting.

### 10.6 CLI (`dbsloc atlas`)

- `dbsloc atlas list [--json]`: one row per manifest atlas plus the bundled ones: `name`, `size` (MB, uncompressed), `status` = `bundled` | `installed` | `available`, `path` when installed. JSON keys: `name`, `bytes`, `status`, `path`.
- `dbsloc atlas fetch <name>...`: prints the notice (§10.4) to stderr, then downloads each named atlas in turn with progress on stderr; prints each installed folder on stdout. Exit 0 all installed; 1 any failure (later names still tried); 2 unknown or bundled name (nothing downloaded); 130 interrupted.
- `dbsloc atlas remove <name>`: deletes a downloaded folder (only under `<user data dir>/atlases/leaddbs/`, and only one carrying `.dbsloc-install.json`); a copied Lead-DBS import or a bundled atlas is refused.
- No per-user data folder → error naming `DBSLOC_DATA_DIR`, as `dbsloc backdrop fetch`.

### 10.7 GUI

Beside *Choose directory…* / *Import from Lead-DBS…* of the *Lead-DBS atlases directory* row: **Download atlases…** opens a window with the notice (§10.4), a filter box, and one row per manifest atlas: name, size, status, and *Download* / *Cancel* / *Remove*. One download at a time (other *Download* buttons disabled while one runs), a progress bar, errors shown in the row. A finished row says *Installed — shown in the Atlas set list on the subject's next Reconstruct or Visualize run.* Bundled atlases are listed as *bundled* without a button.

### 10.8 Acceptance checks

1. The embedded manifest parses, has 73 atlases, every `sha256` is 64 lowercase hex, every path passes §10.2, names are unique, `DISTAL Minimal (Ewert 2017)` is present (and is refused for download as bundled).
2. Against a local HTTP server serving a synthetic zip with `Range` and `ETag` (stored and deflated members, two atlases, one non-atlas member between them, ZIP64 end record in one case): one atlas installs byte-identical with its install record; only that atlas's byte runs are requested (the server log shows no request outside them); a flipped byte in a member → SHA-256 or CRC error, nothing installed; an extra or missing member → "package changed" error; a member path with `..` in the manifest → manifest refused; a server without `Range` → partial-download error; a connection dropped mid-run resumes and succeeds; a changed ETag on resume aborts; cancel leaves no folder and no staging.
3. Discovery: a downloaded folder appears as `user:<name>`; with the external root equal to the user-data root it appears once as `external:<name>`; a name present in two different roots keeps both ids.
4. CLI exit codes of §10.6; `remove` refuses a folder without the install record.
5. Not in CI: one real download of a small atlas (e.g. `PPN_Atlas (Alho 2017)`, 0.4 MB) from the Lead-DBS host, run by hand.

### 10.9 Implementation decisions (Phase C, 2026-11-11; normative where §10.2–§10.6 left a choice open)

- **Manifest checks beyond §10.2:** atlas names are safe folder names; `sha1` is 40 lowercase hex; an atlas has at least one file and its `bytes` equals the sum of its files; no pinned path is `.dbsloc-install.json`; the URL is `http` or `https`.
- **HEAD (§10.3 step 2):** a failed `HEAD`, a missing `Content-Length` or a 3xx answer is a host error; redirects are not followed.
- **Directory (§10.3 step 3):** a central-directory uncompressed size that differs from the pin is "package changed" before any data is fetched.
- **Runs (§10.3 step 4):** the atlas's own directory entries belong to its runs, so they do not split a run. Every run request carries `If-Range` (the HEAD `ETag`, else `Last-Modified`); a 206 whose `ETag` differs from the HEAD value aborts. "At most 5 attempts" means the first request plus up to five retries with the listed waits.
- **Install record (§10.3 step 7):** `files` is the number of files; `installed` is an RFC 3339 UTC timestamp.
- **Existing folder (§10.3 step 6):** a folder `<user data dir>/atlases/leaddbs/<name>/` without an install record (for example a copied Lead-DBS import, possibly edited) is never replaced: the download refuses with a message to remove or rename it first. Only a folder with a record is replaced.
- **Integrity extras (§10.3 steps 4–6):** an atlas entry whose data would be empty or overlap the next entry is a broken package; before the install record is written, the number of extracted files must equal the number of pins; every file and the record are synced to disk before the folder is renamed into place.
- **Validators (§10.3 step 4):** a weak `ETag` (`W/"…"`) is not used for `If-Range`; `Last-Modified` is used instead. A 206 that ends before the requested end (its `Content-Range` end) is continued from the current byte without counting as a retry.
- **Paths (§10.2):** pinned paths and atlas names are also refused when a component is a Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, with or without extension), ends in `.` or a space, or contains `<>"|?*` or a control character; within one atlas no two paths may be equal when compared in lower case.
- **Stalls and renames:** a run is fetched in range requests of at most 32 MB, each with a receive timeout from its own length (at least 120 s, at 50 kB/s, so at most about 11 minutes), so a stalled connection becomes a retry and the cancel flag is seen within that time. The renames of §10.3 step 6 are retried a few times with a short wait (a virus scanner may hold a just-written file on Windows) before the verified staging folder is given up.
- **GUI (§10.7):** closing the window does not cancel a running download; the *Lead-DBS atlases directory* row shows it, and quitting the application cancels it. *Remove* is disabled while a download runs and *Download* while a removal runs. Row errors have a *Copy* button (`docs/agent-rules/gui-ux.md`).
- **Housekeeping:** `remove` takes the same download lock as `fetch`. Under the lock, at the next download or removal: leftover `.removing-*` folders (in `atlases/leaddbs/`) are deleted; `.staging-*` folders (in `atlases/`) older than one day are deleted; for each atlas name with `.partial-old-<name>-*` folders (the previous install set aside during a replace), if `<name>/` is missing the newest of them is renamed back to `<name>/` first (recovery after a failed rollback or a crash between the two renames), and the remaining ones are deleted only while `<name>/` exists. A failed rollback's error names the kept folder. The central directory read is capped at 16 MB. A name given twice to `dbsloc atlas fetch` is downloaded once.
- **CLI (§10.6):** `remove` exits 0 and prints the removed folder, 1 on any refusal. `list` shows a folder in the per-user atlases folder without an install record (for example a copied Lead-DBS import) as `installed`; it cannot be removed with `remove`.
- **Code:** `crates/dbsloc-pipeline/src/atlas_fetch.rs` (+ `atlas_fetch/tests.rs`, own local test server), `crates/dbsloc-cli/src/cmd/atlas.rs`, `crates/dbsloc-cli/tests/cli_atlas.rs`; the download lock reuses `connectome_fetch::DownloadLock`.

