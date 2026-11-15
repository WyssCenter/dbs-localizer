# Spec 06 — Directional Lead Orientation (Wyss DBS Localizer)

**Status:** Implemented and experimental for Boston Scientific and Abbott directional leads on post-op CT (§10); orientation of Medtronic SenSight B33005 / B33015 is planned (§10.9).
**Depends on:** `00` (stage contract, invalidation), `01` (raw post-op CT, transform file names), `03` (CT → anchor transform), `04` (point warp to template space), `05` (reconstruction file, markers, contacts, manual correction), `09` (electrode catalogue), `12` §11.5 (roll convention).

---

## 1. Purpose and scope

The `orientation` stage runs after `reconstruct` (`00`; `--no-orientation` drops it). For every side with a directional (segmented) lead it estimates the **roll**, the rotation of the lead about its own axis, from the artefact of the orientation marker in the post-op CT, following the DiODe approach (§4). It writes the result into the reconstruction file: the `x` / `y` markers of the native, template-space (`mni`) and `reference` blocks, the contacts derived from them, and `meta.orientation` (§10.6).

Boston Scientific Vercise Directed, Vercise Cartesia HX, Vercise Cartesia X and Abbott Directed 6172 / 6173 are estimated; every other directional model, Medtronic SenSight included, reports `unsupported_model`, and ring leads report `not_directed` (§10.2). Per estimated side the stage refits the lead axis to the metal (§10.3a), finds the marker axis from the second angular harmonic of the CT around the lead in the axial slices through the marker (§10.4), chooses between its two directions with the centre of mass of the metal (§10.5), and maps the result back to anchor-native space (§10.6).

- Post-op CT only; for a subject whose post-op modality is MRI the stage is skipped (§10.1).
- The method uses the orientation marker only. It does not analyse the artefacts of the segmented contact levels.
- The implementation is independent, written from the specifications and the papers of §4.
- Not covered here: setting the roll by hand (`05` §9a) and the *Refine localization* view (`08` §11.1.6).

---

## 2. Inputs

| Input | Source |
| --- | --- |
| Reconstruction file (Reco v2 JSON) | native `head` / `tail` per side, `meta.manual_correction` (`05` §8, §9a) |
| Electrode model per side | `props[<k>].elmodel`, else `meta.elmodel`, else the run's model (`--el-model`, else `uiprefs.mat`); catalogue fields `isdirected`, `markerpos`, `markerlen`, lead diameter (`09`) |
| Raw post-op CT | `postop_ct_raw` (`01` §6.2), full resolution, Hounsfield units (HU) |
| CT → anchor transform | `coregistration/transformations/sub-<id>_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat` (`01` §7a, `03`) |
| Run option | `overwrite_approved` (`--overwrite-approved`, GUI *Rerun all selected steps*) |

Processing details: §10.1.

---

## 3. Conventions (normative)

- Side keys: `"1"` = right, `"2"` = left (`05` §8.3).
- `head` is the centre of the most distal contact level and `tail` the tail fiducial (`09` §2.2). `u = unit(tail − head)` points proximally.
- The `y` marker points to **segment A**. For the supported models this is also the direction of the orientation marker (`09` §2.2, §3.1). `x = y × u`, so `x`, `y`, `u` form a right-handed frame. Both markers lie at radius `r` = lead diameter / 2 from `head`, perpendicular to the lead.
- **Roll** is the horizontal azimuth of `y` (§10.6, `12` §11.5): 0° when segment A points anterior, positive toward −x (the patient's left), range (−180°, 180°].
- The marker centre lies `s_m` mm proximal of `head` along `u` (§10.4).
- The CT is analysed in CT world coordinates (RAS mm of the raw CT affine). Results are mapped back to anchor-native space (§10.1, §10.6).

---

## 4. References

- Hellerbach A et al. (2018). DiODe: Directional Orientation Detection of Segmented DBS Leads: A Sequential Algorithm Based on CT Imaging. *Stereotact Funct Neurosurg* 96(5):335–341. doi:10.1159/000494738.
- Dembek TA et al. (2021). DiODe v2: Unambiguous and Fully-Automated Detection of Directional DBS Lead Orientation. *Brain Sci* 11(11):1450. doi:10.3390/brainsci11111450 (marker artefact analysed in the native CT slices; 180° ambiguity resolved by the marker's centre of mass, 2000 HU).
- Kurtev-Rittstieg R et al. (2020). Orientation of Directional DBS Leads on CT: Resolving the Ambiguity. bioRxiv 10.1101/2020.09.16.298653 (intensity profile around the lead–slice intersection in every slice through the marker, 1° steps; centre of mass at 2000 HU on 0.1 mm perpendicular reformats).

---

## 10. Wyss DBS Localizer implementation (normative, 2026-10-12)

Sections 5–9 are unused: §10 keeps the number that other documents cite.

Method `diode_marker_v1`. Estimator: `crates/dbsloc-reconstruct/src/orientation.rs` (`fn support`, `fn refit_axis`, `fn estimate_marker_orientation`, `fn roll_azimuth_deg`, `fn template_marker_directions`). Stage: `crates/dbsloc-pipeline/src/orientation_stage.rs` (`fn run_orientation`). The method is derived from the physics of the marker streak artefact described in the papers of §4. Status: experimental (§10.8).

### 10.1 Inputs

- **Raw CT** `postop_ct_raw`. The stage reads only a sub-region of the volume (`fn ct_region`): it contains, for each estimated lead, the reconstructed axis from `min(0, s_m − markerlen/2 − 1)` to `max(s_m + markerlen/2 + 1, |tail − head|, 20)` mm from `head` with a margin of at least 10 mm, and the estimate equals the whole-volume estimate (§10.7). Non-finite voxel values read as −1024 HU. Interpolated samples are trilinear and clamped to [−1024, 3071] HU; samples outside the volume read −1024 HU. Error: `orientation: cannot read the raw CT <path>: <error>`.
- **CT → anchor transform.** The first transform in the `.mat` file must be an ITK affine `T` (fixed = anchor, moving = CT): it maps an anchor physical point (LPS) to a CT physical point (LPS). With `L = diag(−1, −1, 1)`, an anchor-native RAS point `p` maps to the CT RAS point `L·T(L·p)`, and a CT RAS direction `d` maps back to anchor-native RAS as `L·A⁻¹·L·d`, `A` = linear part of `T`. Errors: `orientation: cannot read the coreg transform <path>: <error>`, `orientation: <path> holds no affine transform`, `orientation: the coreg transform <path> is not invertible: <error>`.
- **Reconstruction file.** Native `head` / `tail` per side (anchor-native RAS mm). A file written before the tail-fiducial convention is converted on read, with a warning (`05` §8.8). Errors: `orientation: cannot read the Reco JSON <path>: <error>`, `orientation: cannot parse the Reco JSON <path>: <error>`, `orientation: missing native markers in the Reco JSON: <path>`.
- **Electrode model** per side as in §2, from the catalogue (`09`): `isdirected`, `markerpos`, `markerlen`, lead diameter. When the run has no model, the stage reads it from `uiprefs.mat` like `reconstruct` does.
- **Post-op modality.** When it is MRI the stage returns `skip:orientation: post-op MRI not supported (specs/06)` before reading anything. A post-op modality of CT or none proceeds.

### 10.2 Supported models

`fn support`: a model is supported when it is directional, its display name starts with `Boston` or `Abbott` (case-insensitive) and both `markerpos` and `markerlen` are defined. The catalogue flag `isdio_supported` is not used (`09` §8).

| Family | Models | Status |
| --- | --- | --- |
| Boston Scientific | Vercise Directed, Vercise Cartesia HX, Vercise Cartesia X | supported (experimental) |
| Abbott | Directed 6172, Directed 6173 | supported (experimental) |
| Medtronic SenSight | B33005, B33015 | `unsupported_model`: the two-piece SenSight marker needs its own method (planned, §10.9) |
| other directional models | e.g. Aleva directSTIM Directed | `unsupported_model` |
| `isdirected = 0` | ring leads | `not_directed` |

For the supported families the orientation marker faces segment A, the `+y` direction of the model frame (`09` §3.1).

### 10.3 Geometry in CT world (RAS)

`h`, `t` = head and tail mapped into CT world (§10.1); `u = unit(t − h)` points proximally. `n` = unit third column of the CT voxel → world affine (the slice-stacking direction, which is the slice normal for an affine without shear); `e1` = unit first column; `e2 = n × e1`. **Polar angle** `α = acos(|u·n|)` in degrees, on the reconstructed axis. `α > 55°` adds the flag `high_polar_angle`; the result is still computed.

### 10.3a Axis refit (normative, 2026-11-14)

Before §10.4, `h` and `u` are refitted to the metal of the lead, because the marker's centre-of-mass signal (about 0.16 mm off the axis in the Lead-DBS lead meshes) is smaller than the lateral error of a reconstructed line. This matches Lead-DBS.

1. For `s = 0, 0.5, …, 20` mm, take the CT axial slice (voxel index along the third axis) nearest to `h + s·u`. Each distinct slice counts once; slices outside the volume are not counted.
2. Per slice: the centroid (world mm) of the voxels `≥ 2000` HU whose centres lie within 3 mm of the point where that slice plane meets the line. When more than half of the visited slices have no such voxel, the threshold drops to `1000` HU for all of them.
3. Fit a straight line to the centroids, one per slice, by least squares (principal axis). The refitted `u` is its direction, oriented like the old `u`; the refitted `h` is the point of the fitted line nearest to the old `h`.
4. With fewer than 5 centroids, a degenerate fit (no principal direction), a refitted `u` more than 10° from the old one, or a lead parallel to the slices, `h` and `u` are kept and the flag `axis_refit_unavailable` is set.

§10.4 and §10.5 use the refitted `h`, `u`. The written markers keep the reconstructed head and tail; only `x` / `y` change (§10.6), and the chosen direction is projected perpendicular to the reconstructed axis before §10.6 writes them. `marker_slice_mm` and `com_offset_mm` are measured along and across the refitted axis from the refitted `h`; `polar_angle_deg` uses the reconstructed axis.

### 10.4 Marker axis (axial slices)

**Marker offset (normative, 2026-11-14).** The marker centre lies `s_m = markerpos − z_1` mm proximal of the head along `u` (`electrode_catalog.rs`, `fn marker_offset_mm`): `markerpos` is measured from the tip apex and `z_1` is the apex-to-centre distance of the most distal contact level (`09` §10.1). The values per model are listed in `09` §2.2. This matches Lead-DBS and puts the marker centre on the marker of the Lead-DBS lead meshes.

1. For every CT slice index `k` whose plane meets the line `h + s·u` at `s ∈ [s_m − markerlen/2 − 1, s_m + markerlen/2 + 1]` mm, `c_k` = that intersection point. Slices outside the volume, and slices where `c_k` falls outside the in-plane extent of the volume, are skipped.
2. Angular profile `P_k(φ_j)`, `φ_j = j°`, `j = 0 … 359`: the mean of the CT samples at `c_k + r·(cos φ_j·e1 + sin φ_j·e2)` over `r = 2.00, 2.25, …, 6.00` mm.
3. `C_k = Σ_j (P_k(φ_j) − mean P_k)·exp(−2iφ_j)`; amplitude `A_k = 2|C_k| / 360` (HU). The slice with the largest `A_k` is used: `marker_slice_mm` = its `s`, `harmonic_amplitude` = `A_k`.
4. Axial direction `d = cos(β)·e1 + sin(β)·e2` with `β = −arg(C_k)/2`. With the `exp(−2iφ)` kernel, `arg(C_k) = −2ψ` for a profile `cos 2(φ − ψ)`, so β is the axis of the profile maxima, perpendicular to the two dark streaks. Marker axis perpendicular to the lead: `m = unit(d − u·(d·u))`, the vector ⟂ `u` whose projection along `u` onto the slice plane is `d`. Candidates: `m` and `−m`.

Errors (the side gets status `failed`, §10.6): `|t − h| < 1e-6` mm → `head and tail coincide`; a singular CT affine → `the CT voxel-to-world affine is singular` (both before §10.3a); no slice in step 1 → `no CT slice through the marker region lies inside the volume`; best `A_k` not finite or below 1e-9 → `the angular profile of the marker slices has no two-fold component`; `|d − u·(d·u)| < 1e-6` → `the marker axis is parallel to the lead`.

### 10.5 Half-plane choice (centre of mass)

`c_m = h + s_k·u` with `s_k` = `marker_slice_mm` (§10.4 step 3). With an orthonormal pair `a, b ⟂ u`, sample the CT in the single plane ⟂ `u` through `c_m` at `c_m + g1·a + g2·b`, `g1, g2 ∈ [−5, 5]` mm in 0.1 mm steps, and keep the samples `≥ 2000` HU. `v` = mean offset of the kept samples from `c_m`, projected ⟂ `u`; `com_offset_mm = |v|`.

- `|v| ≥ 0.05` mm: the candidate with `m·v ≥ 0` is chosen. Status `resolved` when the angle between `v` and the chosen candidate is below 60°, else `ambiguous`.
- `|v| < 0.05` mm: `m` is kept, status `ambiguous`, flag `com_unavailable`.
- No sample reaches 2000 HU: as above, and `com_offset_mm` is `null`.

### 10.6 Outputs

**(a) Native markers and contacts.** For statuses `resolved` and `ambiguous`: the chosen direction (§10.5) is projected ⟂ the reconstructed CT axis and normalised (the candidates likewise). It is mapped to anchor-native space with `L·A⁻¹·L` (§10.1), projected ⟂ `u_n = unit(tail − head)` of the native markers and normalised to `y_dir`; `x_dir = y_dir × u_n`. Native markers: `x = head + r·x_dir`, `y = head + r·y_dir`, `r` = lead diameter / 2; `head` and `tail` are unchanged. The native `coords_mm` of every such side are then recomputed from the new markers with the marker affine of `05` §6.3 (rotation, translation, stretch along the lead), one row per catalogue contact, so directional contacts follow the roll; a PaCER side goes from one row per level to one row per contact, and the previous rows are not kept. This matches Lead-DBS after its orientation step. When the rows cannot be built (no model, model not in the catalogue, a marker missing, a degenerate marker frame) they are kept and the run logs `dbsloc: warning: orientation: native side <k>: contacts kept (<why>)`.

**(b) Template-space blocks (Lead-DBS compatible; owner decision 2026-10-13: Lead-Tutor is the ground truth for the convention).** Template `x` / `y` are not point-warped (`fn template_marker_directions`). Let `y_n` = native `y_dir`. Drop its superior component and renormalise: `ŷ = (y_n,R, y_n,A, 0)/‖·‖`. With `H`, `T` = the block's own `head` / `tail` (already in that space) and `t = unit(T − H)`: `x̃ = (ŷ_A, −ŷ_R, 0)` (`ŷ` turned −90° about +S; for a roll θ, `ŷ = (−sin θ, cos θ, 0)` and `x̃ = (cos θ, sin θ, 0)`), `x_dir = unit(x̃ − (x̃·t) t)`, `y_dir = t × x_dir`; markers `x = H + r·x_dir`, `y = H + r·y_dir`. The azimuth of the native segment direction is reused unchanged as an azimuth in template axes: the rotation of the native → template warp about the vertical is not applied to the roll, as in Lead-DBS, so template `x` matches Lead-Tutor scoring; the physically warped direction differs by that rotation (typically ≤ 20°).

- **Every native → template warp** builds template `x` / `y` this way from the native `y` for every side, whatever produced the native roll (the reconstruction's default frame, this stage or a manual correction): `reconstruct` (TRAC/CORE and PaCER) and the manual-correction re-warp (`05` §6.2, §9a). The `reference` block is not built from the native `y`: it is mapped from the `mni` sides and its `x` / `y` follow this construction from the `mni` `y` (`space_bridge.rs`, `fn reference_block`, `fn warp_sides`, `fn construct_template_markers`; `05` §8.5); the native `y` is applied to it afterwards only for the sides the orientation stage estimated or a manual correction edited (order below, step 4). Without it, a side whose roll is not estimated would keep sheared, non-perpendicular warped markers.
- **Vertical case.** When the horizontal part of `y_n` is shorter than 1e-6, or the template lead is degenerate (`|T − H| < 1e-6`, or `x̃` parallel to `t` so that its component ⟂ `t` is shorter than 1e-6), no markers are built:
  - orientation stage: the side keeps its previous `mni` `x` / `y` and gets the flag `vertical_marker` in `meta.orientation[<k>].flags`; in the `reference` block it keeps `x` / `y` without a flag;
  - `reconstruct`: the side keeps the individually warped `x` / `y` and the flag goes to `meta.reconstruct.flags[<k>]`, with the warning of `05` §6.2;
  - manual-correction re-warp (`05` §9a): the side keeps the individually warped `x` / `y` of the new geometry; the flag `vertical_marker` is added to `meta.orientation[<k>].flags` when that entry has a `flags` list, otherwise the correction reports the warning `<Right lead|Left lead>: vertical_marker: the segment marker is vertical, template-space x / y kept` (printed by `dbsloc reco correct` as `dbsloc: warning: reconstruct: <warning>`); a native-space correction of a file with an `mni` block also records the flag in `meta.reconstruct.flags[<k>]` (`manual_correction.rs`, `fn flag_vertical`, `fn set_reconstruct_flags`).

Order in the orientation stage, after (a):

1. If the file has an `mni` block, each estimated side's `mni` `x` / `y` are rebuilt as above. A missing side or missing `head` / `tail` fails the stage: `orientation: the Reco JSON has no mni markers for side <k>`, `orientation: the mni markers of side <k> lack head / tail`.
2. The rebuilt native contacts are re-warped through the subject's point warp into `mni` `coords_mm_warped` (`05` §6.3). When no warp is available, `coords_mm_warped` is dropped for those sides and the run logs `dbsloc: warning: orientation: individually mapped template contacts (coords_mm_warped) dropped for the re-oriented side(s) (<why>)`.
3. The `mni` `coords_mm` and `trajectory` are recomputed from the markers (`05` §6.3).
4. If the file has a `reference` block, it is rebuilt (`05` §8.5) when the normalization template exists; otherwise the run logs `dbsloc: warning: orientation: no template to rebuild the reference block; its x / y markers and contacts are updated in place` and drops `coords_mm_warped` there for the sides whose native contacts were rebuilt. Either way the estimated sides' `reference` `x` / `y` follow the construction above, and its contacts are recomputed.

**(c) `meta.orientation`.** An object keyed by side (`"1"`, `"2"`), one entry for every side in `native`, replacing the previous value (the TRAC/CORE and PaCER writers leave a string note there, `05` §8.9). Numbers are `null` where not computed.

| Field | Content |
| --- | --- |
| `status` | `resolved`, `ambiguous`, `not_directed`, `unsupported_model` or `failed`; a kept entry may be `manual` |
| `method` | `diode_marker_v1` (`manual` for a manual entry) |
| `roll_deg`, `candidates_deg` | roll and both candidates (below) |
| `polar_angle_deg` | `α` (§10.3) |
| `marker_slice_mm`, `harmonic_amplitude` | §10.4 step 3 |
| `com_offset_mm` | §10.5 |
| `flags` | any of `high_polar_angle`, `axis_refit_unavailable`, `com_unavailable`, `vertical_marker` |
| `software` | `{version, git_hash}` of the writing program |
| `message` | `failed` only: `no electrode model is recorded for this side`, `electrode model "<name>" is not in the electrode catalogue`, `no native head / tail markers`, or an estimator error of §10.4 |

Implementation status: for a kept side whose previous entry is not an object and whose native markers give no roll, `fn kept_entry` writes status `manual` through `fn entry`, which sets `method` to `diode_marker_v1`; the entries the stage builds for kept sides carry no `software`.

`roll_deg` is the horizontal azimuth of `d = y_dir` in anchor-native RAS (decision 2026-10-22): `roll_deg = atan2(−d_R, d_A)` in degrees, in (−180, 180] (0 = segment A anterior, positive toward −x), the same construction as `12` §11.5 and the roll of `05` §9a (`fn roll_azimuth_deg`). `candidates_deg` holds both candidates in the same convention. When the horizontal part of `y_dir` is shorter than 1e-6, the value falls back to the signed angle about `+u_n` (right-hand rule) from `a_ref` to `y_dir`, where `a_ref` is anchor RAS anterior `(0, 1, 0)` projected ⟂ `u_n` and normalised, or RAS `(1, 0, 0)` projected likewise when that projection is shorter than 1e-6 (`fn roll_from_anterior_deg`). The two conventions agree on a vertical lead and differ with lateral tilt (4.1° at 30° tilt, roll 45°).

**Manually set rolls are kept (2026-10-16).** A directional side (supported or `unsupported_model`) whose `roll_review` is `set` or `accepted` (`05` §9a) is not estimated: its native and template markers and contacts stay as they are, and its `meta.orientation[<k>]` entry is carried over; when that entry is not an object, a `manual` entry with the roll of its native markers is written. With `overwrite_approved` every directional side is estimated again, the run logs `dbsloc: warning: orientation: Rerun all selected steps estimates the roll again; the manually set or accepted roll of the <right|left> lead is replaced` (`right and left` when both), and those sides' `roll_review` is set to `null`.

**(d) Stage result.** `ok:orientation:<n>/<d>`: `d` = directional sides (supported or `unsupported_model`), `n` = sides `resolved`, `ambiguous` or kept. The stage skips, leaving the file untouched and reading no CT, in two cases:

- No side is directional: `skip:orientation: no directional leads (<detail>)`. `<detail>` is `<model> has ring contacts only` when every side has the same ring model (also with a single side); `right: <A>, left: <B>; ring contacts only` when the ring models differ; otherwise per side `<side>: <reason>` joined by `; `, where `<reason>` is `<model> has ring contacts only` or the `failed` message of a side whose electrode model is missing or not in the catalogue; `the reconstruction has no lead sides` when there is none.
- At least one directional side is kept and no side is estimable, for example one kept side next to an `unsupported_model` side: `skip:orientation: the roll of every directional lead was set or accepted in a manual correction (specs/05 §9a)`. No `unsupported_model` entry is written in that case.

Otherwise the raw CT and the CT transform are required, also when every remaining directional side is `unsupported_model`; a missing Reco JSON, raw CT or CT transform fails the stage with `orientation: missing <what>: <path>` (`Reco JSON`, `raw CT`, `coreg transform`). Log lines: `[orientation] raw CT <file> through <transform file>`, `[orientation] <right|left>: <status>`, `[orientation] written: <file>`. The Reco JSON is written through `.partial-<name>` and a rename (`crates/dbsloc-pipeline/src/outputs.rs`, `fn write_replacing`), with `meta.tail_fiducial` set (`05` §8.8), and its MATLAB twin is written beside it (`05` §8.10). Completing the stage invalidates `vat` and `visualize` (`00` §6.1). Consumers: the *Refine localization* view (`08` §11.1.6, `05` §9a) shows the `status`, *Other candidate* switches to the other value of `candidates_deg` and *Automatic* returns to the roll stored in the file; directional contacts in every later stage follow the `x` / `y` markers.

### 10.7 Acceptance

Unit tests in `orientation.rs` and `orientation_stage.rs` (module `tests`):

- **Phantom.** Synthetic CT, 0.45 × 0.45 × 1 mm voxels, background 30 HU. Metal cylinder (3071 HU, partial volume from 27 sub-samples per voxel) of radius 0.65 mm from 3 mm distal to 25 mm proximal of the head. Marker band: a 140° arc facing segment A, wall 0.30 mm (radius 0.65–0.95 mm), length `markerlen`, centred `s_m` proximal of the head. Two dark streaks (−600 HU, Gaussian σ 0.6 mm across, decaying as `exp(−d / 5 mm)` with the distance `d` along the in-slice tangent perpendicular to the marker axis) in the slices within the band, tapered along the lead by `exp(−((s − s_m)/0.75 mm)²)`. Leads at polar angles 0°, 30°, 50° and rolls 0°, 75°, −130°: roll within 3° (so the correct half is chosen), no flags, the two candidates 180° apart, `x ⟂ u`, `x = y × u`. The 0.30 mm wall puts the single-plane centre-of-mass offset at 0.08–0.18 mm, near the 0.16 mm of the Lead-DBS meshes; a 0.15 mm wall would sit at the 0.05 mm threshold.
- **Missing centre of mass.** Metal at 1500 HU: `ambiguous`, `com_unavailable`, `com_offset_mm` null, axis within 3° modulo 180°. Metal at 900 HU also sets `axis_refit_unavailable`.
- **Axis refit.** A 20°-tilted rod (radius 0.7 mm, 3000 HU) started 5° and 0.8 mm off: refitted axis within 1°, head within 0.3 mm of the rod axis and nearest to the old head. A 1500 HU rod refits through the 1000 HU fallback; a 900 HU rod does not; a start 20° off is rejected.
- **Single plane.** Extra metal 6.5 mm off the axis toward segment A in the marker band raises `com_offset_mm` by more than 0.15 mm and stays `resolved` within 3°; a decoy 1 mm off the marker slice plane changes nothing.
- **Limits.** Polar angle 60° → `high_polar_angle`, still computed. A lead lying in the slice plane → `no CT slice through the marker region lies inside the volume`. `head = tail` → `head and tail coincide`.
- **Transform round trip.** Head / tail through the CT map and back within 1e-6 mm; mapped directions agree within 1e-9.
- **Mirror phantom.** One CT stored LAS (negative first voxel axis) holding two leads at x = ±12 mm, tilted 15° laterally and 20° anteriorly, mirror images in geometry and roll (rolls ±40°, then the asymmetric pair 40° / −110°), also with the CT affine rotated 10° about S. In the estimator and through the stage: both sides `resolved`, each `roll_deg` within 3° of its truth, rolls of opposite sign (sum within 3° for the symmetric pair), native `|y − head| = r`, template `x` / `y` perpendicular to the template lead with `y = t × x`.
- **Template-space markers.** With an identity native → template map they equal the native markers for a vertical lead; for a rotation of 30° about S the template `x_dir` keeps the native azimuth; `x_dir ⟂ t`, `y_dir = t × x_dir`; a vertical segment direction or a degenerate lead builds none.
- **Roll convention.** `roll_azimuth_deg`: 0° for `y` anterior, 90° for −x, −90° for +x, 180° for posterior, none without a horizontal part; equal to the axis roll for a vertical lead.
- **Stage.** A Boston Scientific side on a phantom CT is `resolved` within 3° of its 75° roll with `software` recorded; a ring side and a Medtronic B33005 side (`unsupported_model`, `ok:orientation:1/2`) keep their native and `mni` blocks unchanged; native contacts follow the rewritten markers and PaCER level rows become one row per contact; ring-only files skip with the exact `<detail>` strings of §10.6 and stay byte-identical; post-op MRI skips; missing inputs fail with the named file; the cropped CT gives the same estimate as the whole volume; stale `coords_mm_warped` of a re-oriented side is dropped with the warning when no warp is available.

### 10.8 Cohort result (Lead-Tutor, 2026-11-14)

Full pipeline on the Lead-Tutor cohort, template-space reconstructions uploaded to Lead-Tutor on 2026-11-14. Rotation error per lead in degrees, as reported by Lead-Tutor (unsigned):

| Model | Leads | Rotation error (°) |
| --- | --- | --- |
| Boston Scientific Vercise Directed | 10 leads, 5 subjects | 14.5, 18.9, 16.7, 5.1, 12.5, 6.3, 15.6, 7.2, 20.1, 16.4 (mean 13.3, max 20.1) |
| Abbott Directed 6172 | 2 leads, 1 subject | 15.6, 15.8 |
| Medtronic B33005 | 6 leads, 3 subjects | not oriented (`unsupported_model`) |

Every Boston Scientific and Abbott lead is `resolved`; both leads of one Boston Scientific subject carry the flag `high_polar_angle`. The localization error is unchanged: the stage moves neither head nor tail.

### 10.9 Planned: Medtronic SenSight B33005 / B33015

**Planned.** Orientation of the two-piece SenSight marker. Known: the marker centre `h + s_m·u` (§10.4, values in `09` §2.2) is the boundary between the distal and the proximal marker piece (B33005: pieces 12.0–14.3 mm and 14.5–16.8 mm from the apex), and the distal piece faces segment B (`09` §2.2). Reported Lead-DBS behaviour, not yet verified in this project: the distal piece is sampled from −4 to 0 mm and the proximal piece from 0 to +4 mm along `u` from that boundary, in 0.2 mm steps, each on a plane perpendicular to the lead reformatted at 0.1 mm, and the decision uses the largest centre-of-mass distance from the axis. Until this is implemented and checked on Lead-Tutor, B33005 / B33015 return `unsupported_model`.

Unspecified: the HU threshold of the centre of mass; how the two pieces' centres of mass become the segment-A direction; what the largest centre-of-mass distance decides; the method name, statuses, flags and acceptance criteria; the piece positions of B33015.
