# Spec 06 — Directional Orientation (DiODe) (Wyss DBS Localizer)

**Status:** Phase B  
**Depends on:** `05_reconstruction`, `09_electrode_models` (`is_directed`, `markerpos`), `03` (CT affines)  
**Phase A (archived):** the Phase A archive (removed 2026-10-23, in git history) — section `specs/phase_a_diode_report.md`

---

## 1. Purpose

Compute **roll** angle of **segmented (directional) leads** around the shaft axis from **post-op CT** artifacts (marker ring, “dark star”, tungsten ring pairs), **per hemisphere**. **MRI post-op = unsupported** (skip or `E_DIOD_MRI`).

**Output:** scalar **roll** (radians or degrees) per side, stored in `reco` / options as implementation attaches; also **3×3** rotation in the plane **perpendicular** to the lead for VAT/stim (see `07`).

**Out of scope:** full 3D viewer UX — **result values** and reproducibility only.

---

## 2. Inputs


| Input                                     | Notes                                                  |
| ----------------------------------------- | ------------------------------------------------------ |
| `ct_coreg`                                | Path — coregistered CT (in anchor native)              |
| `ct_original` / native CT                 | **Preferred** for fine artefact (full resolution)      |
| `T_reg_to_orig`                           | 4×4 — world mm: coreg **→** original CT                |
| `reco.native.markers[side].head` / `tail` | mm along shaft in **coreg** space (then mapped to org) |
| `Elspec`                                  | `is_directed`, `markerpos`, `markerlen`, `elmodel`     |
| `sides`                                   | 1 = R, 2 = L                                           |


**If** `T_reg_to_orig` missing: build from `coreg.transform.CT` forward `.mat` **inverted** (ANTs) or from raw-CT pipeline matrix per `01`.

---

## 3. Coordinate conventions

- **Unit vector** `u = normalize(tail - head)` — **distal to proximal** (or inverted consistently in implementation — **one** docstring).  
- **Perpendicular basis:** `ea_calcxy`-class: two orthonormal vectors in plane ⟂ `u` for RHC frame.  
- Slices: **reformat** oblique to perpendicular plane of lead at the marker centre, `s_m` mm proximal of the head along **+u** (§10.4).

---

## 4. Branching by lead family (normative)


| Family                                    | Method sketch                                                                             |
| ----------------------------------------- | ----------------------------------------------------------------------------------------- |
| **Medtronic B33x** (two tungsten rings)   | Intensity in plane; detect two COGs; **angle** between vector(COG1,COG2) and model offset |
| **Boston / Abbott (marker or dark star)** | FFT angular spectrum **or** 6-lobe **dark star** fit                                      |
| **Manual**                                | User picks reference direction                                                            |
| **Light marker** (full ring)              | High-intensity **centroid** vs model phase                                                |


**Dispatch:** by `Elspec.elmodel` string → `OrientationStrategy` table (data file in `dbsloc`).

---

## 5. Core sub-algorithms (abstract)

**5.1 `intensity_peaks_fft` (angular)**

- Resample **annular** band to polar `(ρ,φ)`; **1D FFT** on φ; dominant peak = **roll** offset vs reference template angle for that model.

**5.2 `dark_star`**

- Binarize **6 rays**; fit **6-way** pattern center and phase offset.

**5.3 `intensity_profile` along arc**

- Sample intensities; **peaks** → segment boundaries.

**5.4 `roll_pitch_yaw` → `rotation_matrix`**

- Convert scalar roll about `u` to `R_w` 3×3 in world (for `coords` in stim).

**5.5 Post-process**

- **Medtronic** re-specify slice plane if `reco` markers moved.

---

## 6. Output

- `**roll_out[side]`** — float, consistent **0 = reference** in model (document per `elmodel`).  
- **Optional** `R_body_to_world[3,3]`.  
- **Persist** in `reco` extension field or `stim` prep — path in **implementation**; **this spec** only requires value + uncertainty flag.

**Uncertainty** / QC: if SNR < threshold → `warning` + NaN roll.

---

## 7. Edge cases

- `postop_modality == MRI` → return early (no error in batch, or `E_DIOD` — pick one, document).  
- **Anisotropic** voxels: use `ct_original` to avoid over-smoothing.  
- **Left/right** mirror in image vs patient: `flip` from NIfTI **affine** (det sign).

---

## 8. Acceptance criteria

- Phantom: synthetic dark-star image → **roll** within **3°** of ground truth.  
- B33 two-sphere phantom → same.  
- `T_reg_to_orig` round-trip: head/tail in org align within **<0.1 mm** with fixture affines.  
- Non-directed lead → `skip` (no `E_` if explicit).

---

## 9. References

- DBS directional programming reviews; manufacturer IFU geometry.  
- Schönecker (orientation markers) as cited in Lead-DBS / DiODe papers.
- Hellerbach A et al. (2018). DiODe: Directional Orientation Detection of Segmented DBS Leads: A Sequential Algorithm Based on CT Imaging. *Stereotact Funct Neurosurg* 96(5):335–341. doi:10.1159/000494738.
- Dembek TA et al. (2021). DiODe v2: Unambiguous and Fully-Automated Detection of Directional DBS Lead Orientation. *Brain Sci* 11(11):1450. doi:10.3390/brainsci11111450 (marker artefact analysed in the native CT slices; 180° ambiguity resolved by the marker's centre of mass, 2000 HU).
- Kurtev-Rittstieg R et al. (2020). Orientation of Directional DBS Leads on CT: Resolving the Ambiguity. bioRxiv 10.1101/2020.09.16.298653 (intensity profile around the lead–slice intersection in every slice through the marker, 1° steps; COM at 2000 HU on 0.1 mm perpendicular reformats).

---

## 10. Wyss DBS Localizer implementation (normative, 2026-10-12)

Derived from the papers of §9 (physics of the marker streak artefact), not from any reference code. Status **experimental**: on the Lead-Tutor cohort the marker axis is within 20° of the expert orientation on 7 of 10 Boston leads; see §10.8.

### 10.1 Inputs

- Raw post-op CT `postop_anat["CT"]` (full resolution; values in HU; NaN read as −1024; samples clamped to [−1024, 3071]).
- CT → anchor transform `coregistration/transformations/sub-<id>_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat` (`01` §7a) (ITK affine, fixed = anchor, moving = CT: it maps an anchor physical point, LPS, to a CT physical point, LPS). Anchor-native RAS point `p` → CT RAS point: `L·T(L·p)` with `L = diag(−1, −1, 1)`.
- Reco v2 `native` markers `head`, `tail` per side (anchor-native RAS mm) and the side's electrode model from the catalogue (`09`): `isdirected`, `markerpos`, `markerlen`, lead diameter.
- Post-op modality other than CT: the stage returns `skip:orientation: post-op MRI not supported (specs/06)`.

### 10.2 Supported models

| Family | Models | Status |
| --- | --- | --- |
| Boston Scientific | Vercise Directed, Vercise Cartesia HX, Vercise Cartesia X | supported (experimental) |
| Abbott | Directed 6172, Directed 6173 | supported (experimental) |
| Medtronic SenSight | B33005, B33015 | `unsupported_model` (the marker-axis method of §10.4 does not track the SenSight artefact: mean error 46° on 6 cohort leads) |
| others with `isdirected` | e.g. Aleva directSTIM | `unsupported_model` |
| `isdirected = 0` | ring leads | `not_directed` |

For the supported families the orientation marker faces **segment A** (the `+Y` direction of the local lead frame, `09` §3.2).

### 10.3 Geometry in CT world (RAS)

`h`, `t` = head and tail mapped into CT world; `u = unit(t − h)` (points proximally). `n` = unit third column of the CT voxel → world affine (slice normal); `e1` = unit first column; `e2 = n × e1`. **Polar angle** `α = acos(|u·n|)` in degrees; `α > 55°` adds the flag `high_polar_angle` (the result is still computed).

### 10.3a Axis refit (normative, 2026-11-14, reference behaviour)

Before §10.4, `h` and `u` are refitted to the metal of the lead itself, because the marker's centre-of-mass signal (about 0.16 mm off the axis in the reference meshes) is smaller than the lateral error of a reconstructed line. For `s = 0, 0.5, …, 20` mm, take the CT axial slice nearest to `h + s·u`; each distinct slice so visited counts once, and slices outside the image are not counted. Per slice: the centroid (world mm) of the voxels `≥ 2000` HU within 3 mm in-plane of the point where that slice meets the line. When more than half of the visited slices have no such voxel, the threshold drops to `1000` HU for all of them. A straight line is fitted to the centroids, one per slice, by least squares (principal axis); the refitted `u` is its direction oriented like the old `u`, the refitted `h` the point of the fitted line nearest to the old `h`. With fewer than 5 centroids (distinct slices), or a refitted `u` more than 10° from the old one, `h` and `u` are kept and the flag `axis_refit_unavailable` is set. §10.4 and §10.5 use the refitted `h`, `u`; the written markers keep the reconstructed head and tail (only `x` / `y` change, §10.6), and the chosen direction is projected perpendicular to the reconstructed axis before §10.6 writes them. `marker_slice_mm` and `com_offset_mm` are measured along and across the refitted axis from the refitted `h`; `polar_angle_deg` uses the reconstructed axis.

### 10.4 Marker axis (axial slices)

**Marker offset (normative, 2026-11-14).** The marker centre lies `s_m = markerpos − z_1` mm proximal of the head marker along `u`. `markerpos` is measured from the tip apex (`09` §2.2) and `z_1` is the apex-to-centre distance of the most distal contact level (`09` §10.1: `tip_length + contact_length/2` for an insulating tip, `tip_length/2` when the tip is the first contact). So `s_m` = 10.25 mm Boston Vercise Directed, 18.15 mm Cartesia HX, 14.15 mm Cartesia X, 9.0 mm Abbott 6172, 12.0 mm Abbott 6173 (12.75 / 15.75 mm Medtronic B33005 / B33015, unsupported here). This is the reference implementation's rule and puts the marker on the marker of the reference meshes. Until 2026-11-14 this implementation used `s_m = markerpos`, 0.75–1.85 mm too far proximal (on Abbott 6172 the centre-of-mass box of §10.5 then missed the marker).

1. For every CT slice index `k` whose plane meets the line `h + s·u` at `s ∈ [s_m − markerlen/2 − 1, s_m + markerlen/2 + 1]` mm: `c_k` = that intersection point.
2. Angular profile `P_k(φ_j)`, `φ_j = j°`, `j = 0 … 359`: mean of trilinear CT samples at `c_k + r·(cos φ_j·e1 + sin φ_j·e2)` over `r = 2.00, 2.25, …, 6.00` mm.
3. `C_k = Σ_j (P_k(φ_j) − mean P_k)·exp(−2iφ_j)`; amplitude `A_k = 2|C_k| / 360`. The slice with the largest `A_k` is used (`marker_slice_mm` = its `s`, `harmonic_amplitude` = `A_k`).
4. Axial direction `d = cos(β)·e1 + sin(β)·e2` with `β = −arg(C_k)/2` (with the `exp(−2iφ)` kernel, `arg(C_k) = −2ψ` for a profile `cos 2(φ − ψ)`; β is the axis of the profile maxima, i.e. perpendicular to the two dark streaks). Marker axis perpendicular to the lead: `m = unit(d − u·(d·u))` (the vector ⟂ u whose projection along `u` onto the slice plane is `d`). Candidates `m` and `−m`.

### 10.5 Half-plane choice (centre of mass)

`c_m = h + s_k·u` with `s_k` = `marker_slice_mm` of §10.4 step 3 (the marker slice found inside the window around `s_m`). Orthonormal `a, b ⟂ u`. Sample the CT in the single plane ⟂ u through `c_m`: `c_m + g1·a + g2·b`, `g1, g2 ∈ [−5, 5]` step 0.1 mm (reference behaviour, 2026-11-14; until then a ±2 mm box ±markerlen/2 along `u` around `h + s_m·u`); keep samples `≥ 2000` HU; `v` = mean of kept points minus `c_m`, projected ⟂ u (`com_offset_mm = |v|`). If `|v| ≥ 0.05` mm the candidate with `m·v > 0` is chosen; status `resolved` when the angle between `v` and the chosen candidate is `< 60°`, else `ambiguous`. If `|v| < 0.05` mm or no sample exceeds 2000 HU: `m` is kept, status `ambiguous`, flag `com_unavailable`.

### 10.6 Outputs

- Segment-A direction in CT world `y_ct = ±m` (§10.5); `x_ct = y_ct × u`. Mapped to anchor-native with the inverse linear part of the CT ↔ anchor map. Native markers: `y = head + r·y_dir`, `x = head + r·x_dir`, `r` = lead radius (diameter / 2), for statuses `resolved` and `ambiguous`. **Template-space markers (Lead-DBS compatible, decision 2026-10-13: Lead-Tutor is the ground truth for the convention).** The `x` / `y` markers are *not* point-warped. Let `y_n` = native `y_dir`; drop its superior component and renormalise, `ŷ = (y_n,R, y_n,A, 0)/‖·‖` (when the horizontal part is shorter than 1e-6, or the template lead is degenerate — `H = T`, or `x̃` parallel to `t` so its component ⟂ `t` is shorter than 1e-6 — the **vertical case**: the orientation stage keeps the side's previous `mni` `x` / `y` and adds flag `vertical_marker` to `meta.orientation[<side>].flags`; on a native → template warp (`reconstruct`, a manual-correction re-warp) the side keeps the individually warped `x` / `y` of the **current** geometry (the markers the warp just produced, not those of an earlier file). `reconstruct` logs `dbsloc: warning: reconstruct: mni side <k>: vertical_marker: …` per side and records the flag in `meta.reconstruct.flags[<k>]` (an array of strings, `05` §8; decision 2026-10-16), which the GUI Reconstruct card shows; a correction adds the flag to `meta.orientation[<side>].flags` when that entry exists, else reports `<side>: vertical_marker: …` as a warning). The azimuth of `ŷ` is reused unchanged as an azimuth in template axes. With `H`, `T` = the side's template-space `head` / `tail` (already warped) and `t = unit(T − H)`: `x̃ = (ŷ_A, −ŷ_R, 0)` (ŷ turned −90° about +S), `x_dir = unit(x̃ − (x̃·t) t)`, `y_dir = t × x_dir`; markers `x = H + r·x_dir`, `y = H + r·y_dir`. The same construction gives the `reference` block markers (`05` §8) from the reference-space `head` / `tail`. **Applies at every native → template warp, not only after orientation (2026-10-15):** `reconstruct` (TRAC/CORE and PaCER), the manual-correction re-warp and the bridge to the `reference` block build template `x` / `y` this way for **every** side from its native `y_dir` (whatever produced the native roll: the reconstruction's default frame, DiODe or a correction), as the expert pipeline does; template `x` / `y` are never point-warped. Without this, a side whose roll is not estimated (ring leads, `unsupported_model`, `unresolved`) kept sheared, non-perpendicular warped markers (cohort: `x̂·û` up to 0.30). `head` / `tail` are unchanged; the template-space (and `reference`) contacts are then recomputed from the new markers (`05` §6, *Template-space contacts from the warped markers*, 2026-10-15), so directional contacts follow the roll. **Native contacts follow the roll too (2026-10-15, reference-implementation behaviour):** for every side whose native `x` / `y` the stage rewrites (`resolved`, `ambiguous`), the native `coords_mm` are recomputed from the new native markers with the same marker affine `T` (`05` §6, applied to the `native` block: rotation, translation, stretch `s` along the lead), one row per catalogue contact; a PaCER side therefore goes from one row per level to one row per contact, as in the expert pipeline after its orientation step. The previous native rows are not kept. Degenerate markers keep the native rows with `dbsloc: warning: orientation: native side <k>: contacts kept (…)`. Sides the stage does not rewrite keep their native contacts. Consequence: the native → template rotation about the vertical is not applied to the roll (as in the expert pipeline), so template-space `x` matches Lead-Tutor scoring; the physically warped direction differs by that rotation (typically ≤ 20°).
- `roll_deg` (**horizontal azimuth, decision 2026-10-22; was the rotation about `u` from `a_ref` until then**): with `d = y_dir` in anchor RAS, `roll_deg = atan2(−d_R, d_A)` in degrees, in `(−180, 180]` (0 = segment A anterior, positive toward −x / the left, the same construction as `12` §11.5 and the `05` §9a roll); `candidates_deg` = both candidates in the same convention. When the horizontal part of `y_dir` is shorter than 1e-6 the value falls back to the signed angle from `a_ref` (anchor RAS anterior `(0, 1, 0)` projected ⟂ u, normalised; RAS `(1, 0, 0)` instead when that projection is shorter than 1e-6) to `y_dir` about `+u` (right-hand rule). The two conventions agree on a vertical lead and differ by the lateral tilt otherwise (4.1° at 30° tilt, roll 45°). A `manual` entry written by a correction (`05` §9a) holds the azimuth the expert gave.
- `meta.orientation` becomes an object keyed by side (`"1"`, `"2"`): `{status, method: "diode_marker_v1", roll_deg, candidates_deg, polar_angle_deg, marker_slice_mm, harmonic_amplitude, com_offset_mm, flags}`; statuses `resolved`, `ambiguous`, `not_directed`, `unsupported_model`, `failed` (with `message`). Numbers are `null` where not computed. The Reco JSON is rewritten through `.partial-*` + rename.
- **Expert rolls are kept (2026-10-16).** A directed side (supported or `unsupported_model`) whose `meta.manual_correction[<k>].roll_review` is `set` or `accepted` (`05` §9a *Per-side review*) is not estimated: its native and template markers and contacts stay as they are and its `meta.orientation[<k>]` entry is carried over (when it is not an object, a `manual` entry with the roll of its native markers is written). The other sides are estimated as above. When no side is left to estimate the stage returns `skip:orientation: the roll of every directional lead was set or accepted in a manual correction (specs/05 §9a)` without reading the CT. With `overwrite_approved` (*Rerun all selected steps*) every directed side is estimated again, the run logs `dbsloc: warning: orientation: Rerun all selected steps estimates the roll again; the manually set or accepted roll of the <right|left> lead is replaced`, and those sides' `roll_review` is set to `null`.
- Stage result: `ok:orientation:<resolved+ambiguous+kept>/<directed sides>`; `skip:orientation: no directional leads (<detail>)` when no side is directed; `<detail>` says why per side: `<model> has ring contacts only` when both sides share one ring model, `right: <A>, left: <B>; ring contacts only` when they differ, `<side>: <reason>` (joined by `; `) for a side whose electrode model is missing or not in the catalogue, `the reconstruction has no lead sides` when there is none. Missing inputs (no Reco JSON, no raw CT, no coreg transform) fail the stage with `orientation: missing <what>: <path>`.

### 10.7 Acceptance

- Phantom (synthetic CT, 0.45 × 0.45 × 1 mm, HU background 30): metal cylinder `r = 0.65` mm at least 25 mm long, marker band 140° arc of 0.30 mm wall centred `s_m` (§10.4) proximal of the head (2026-11-14: a 0.15 mm wall puts the single-plane centre-of-mass offset of §10.5 at 0.02–0.10 mm, at the 0.05 mm threshold; 0.30 mm gives 0.08–0.18 mm, near the 0.16 mm of the reference meshes), plus two dark streaks (−600 HU, Gaussian width 0.6 mm, fading with distance) along the in-slice tangent at the marker's middle, their strength tapered along the lead by `exp(−((s − s_m)/0.75 mm)²)`; leads at polar angles 0°, 30°, 50° and roll 0°, 75°, −130°: `roll_deg` within 3° and the correct half chosen.
- Transform round trip: head / tail through the CT map and back within 1e-6 mm.
- Mirror phantom: one CT stored LAS (negative first voxel axis) holding two leads at x = ±12 mm, tilted laterally 15° and anteriorly 20°, mirror images of each other in geometry and roll (rolls ±40°, then an asymmetric pair 40° / −110°): each `roll_deg` within 3° of its truth, `roll_deg` of the mirrored pair of opposite sign, correct half chosen on both sides; the same with an oblique CT affine (10° about S).
- Template-space markers (§10.6): with an identity native → template map they equal the native markers for a vertical lead; for a pure rotation of 30° about S the template `x_dir` keeps the native azimuth (not rotated); `x_dir ⟂ t`, `y_dir = t × x_dir`.
- `not_directed` and `unsupported_model` sides keep their `x` / `y` markers byte-identical.

### 10.8 Cohort result (2026-10-12)

Ground truth: the expert orientation recovered by intersecting two Lead-Tutor uploads per lead (`LeadTutor/hpdbs_validation/experts_orientation.json`, both intersections within 2.7°), assuming segment A = expert `x` rotated +90° about `tail − head`. §10.4 axis error (mod 180°) on the raw CTs, run `main_71d18c3_1mm`: **Abbott** 18°, 1° (2 leads); **Boston** 43, 20, 78, 34, 51, 2, 51, 6, 54, 13° (mean 35°; chance level is 45°); **Medtronic B33005** 49, 30, 69, 11, 15, 70° (hence `unsupported_model`). The §10.5 half-plane choice agreed with the expert on 6 of 10 Boston leads.

**Pipeline run `batch_2026-10-12_precoreg_diode`** (all stages, this implementation, MNI `x` markers vs the recovered expert `x`): left-hemisphere (−x) Boston leads 8, 15, 8, 15, 25° full-circle error, i.e. *expert = ours − 15°* (residuals 7, 0, 7, 40, 0°); right-hemisphere (+x) Boston leads 94, 112, 86, 101, 13°, which fit *expert = −ours − 24°* (a reflection; residuals 24, 35, 0, 10, 11°). Abbott (`39468`): axis within 20° / 7° on both sides, half-plane wrong on both (status `ambiguous`).

**Correction (2026-10-13).** The ground truth above is ambiguous for every lead: Lead-Tutor reports an unsigned rotation error, so each upload leaves two candidates `p ± e` around the submitted angle `p`, and both uploads used for the intersection carried the default orientation (`p` differing by 0.3–9° per lead), so the second candidate fits within 0.4–10° (12–21° on `78754`). The branch choice, and with it the apparent right-hemisphere reflection, is not determined by the data. A Phase B reading of the expert pipeline (Lead-DBS, behaviour only) found one convention for both hemispheres (rotation 0 = segment A anterior, positive counter-clockwise seen from above, `y` = segment A, `x = y × t`, side 1 = right), no per-side sign, Lead-Tutor keys swapped relative to anatomy (`leftElectrode` = side 1 = right hemisphere), and template-space `x` / `y` rebuilt from the native azimuth rather than warped (§10.6). Scored against the better-fitting candidate per lead, the 10 `resolved` Boston leads err 25, 15, 1, 10, 5° (−x) and 13, 12, 54, 46, 86° (+x). Open: a third upload with non-default orientation resolves the branch per lead; a mirrored two-lead phantom (§10.7) checks the +x side independently.

**Third upload (2026-10-15).** Run `hpdbs_runs/final_2026-10-15` (same head / tail as 2026-10-13, DiODe `x` on the oriented leads) was uploaded; its unsigned errors pick one candidate per lead (`LeadTutor/hpdbs_validation/experts_orientation_v2.json`, `resolve_orientation_2026-10-15.py`). Against the resolved expert angles the 10 `resolved` Boston leads err 14.4, 17.3, 12.6, 15.5, 20.2° (+x, Tutor `leftElectrode`) and 19.0, 4.9, 6.2, 7.3, 16.4° (−x): mean **13.4°** (+x 16.0°, −x 10.8°), max 20.2°; no lead is near the 180° / mirrored branch, so the apparent right-hemisphere reflection was an artefact of the ambiguous ground truth. Candidate margins below 5° (57245 −x 3.7°; the B33005 and ring leads, which DiODe does not orient) leave those leads' expert angle uncertain. `39468` (Abbott, `ambiguous`) errs 116° (+x) and 16° (−x): the half-plane choice is still wrong on +x.
