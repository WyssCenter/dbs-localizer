# PaCER — algorithm specification (implementation contract)

**Reference (behaviour, not code):** Husch et al. (2017), *PaCER — A fully automated method for electrode trajectory and contact reconstruction in deep brain stimulation.* *NeuroImage: Clinical* 17, 80–89.  
**Original implementation:** separate codebase, **AGPL-3** (copyleft). This document describes *observable behaviour* and *mathematical structure* so that `crates/dbsloc-reconstruct` can implement a **functionally equivalent** pipeline without translating that source.

**Relationship to `05_reconstruction.md`:** that file defines the **Wyss DBS Localizer pipeline interface** (when to call PaCER, spaces, errors). This file is the **full algorithm contract** for the in-tree Rust implementation.

---

## 1. Scope

- **In scope:** Post-op **CT** volume; automatic detection of **one or two** DBS leads as metal artifacts; estimate 3D **trajectory** as a smooth curve; detect **ring contact** positions along the shaft (and optional x-ray marker region for some leads); output contact locations in **scanner/world mm** (same frame as input NIfTI).
- **Out of scope (optional / later):** Boston Vercise directional **rotation** module if present as separate plugin; GUI; Medtronic XML plan **required** for operation (only **association** when file provided).

---

## 2. Inputs


| Input                      | Type                                                                                  | Notes                                                                                                           |
| -------------------------- | ------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| CT volume                  | 3D scalar, Hounsfield units (typical range about −1024…3071, or offset-positive copy) | Must carry **voxel size** (mm) and **affine** voxel→world (NIfTI qform/sform convention).                       |
| `metal_threshold`          | HU, default **800**                                                                   | If `min(ct) >= 0`, add **1024** to threshold (offset-positive convention).                                      |
| `brain_mask`               | optional binary mask same grid as CT                                                  | If absent, compute mask (§4).                                                                                   |
| `no_mask`                  | bool, default false                                                                   | If true, treat full FOV as valid (phantoms).                                                                    |
| `reverse_dir`              | bool                                                                                  | Flip skeleton order before first poly fit (phantom / odd orientations).                                         |
| `final_poly_degree`        | int ≥ 1, default **3**                                                                | Degree for **final** trajectory polynomial after zero calibration.                                              |
| `contact_detection_method` | enum: `peak`, `peak_wave_center`, `contact_area_center`                               | If `max(voxel_size) > 1` mm, implementations should force `contact_area_center` (independent peaks unreliable). |
| `electrode_type`           | optional enum: `Medtronic3387`, `Medtronic3389`, `BostonVerciseDirectional`, or unset | If set, force `contact_area_center` for contact method.                                                         |
| `medtronic_xml_plan`       | optional file path                                                                    | Trajectory entries used to **label** detected clouds, not required for detection.                               |


---

## 3. Outputs

Per detected electrode **i**:


| Output                           | Meaning                                                                                                                                                                                                                                                  |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `r3_polynomial`                  | Coefficients for parametric curve **p**:[0,1]→ℝ³, **each axis** a real polynomial in **t** (columns: x, y, z coefficients, **highest degree first** matching common `polyval` ordering — document in code).                                              |
| `electrode_info`                 | Identified or user-forced lead model; includes **ring_contact_centers_mm** (arc length from proximal reference along shaft), **zero_to_first_peak_mm**, **diffs_mm** (pairwise spacing model for classification), **diameter_mm** (~1.27 for Medtronic). |
| `use_detected_contact_positions` | bool — whether measured peaks matched prototype well enough.                                                                                                                                                                                             |
| `detected_contact_positions_mm`  | optional arc-length samples along shaft when individual contacts resolved.                                                                                                                                                                               |
| `intensity_profile_1d`           | Smoothed median intensity along refined skeleton (for QC).                                                                                                                                                                                               |
| `skeleton_scale_mm`              | Monotonic arc-length axis (mm from shaft origin) aligned with profile samples.                                                                                                                                                                           |


**Multi-lead:** run the per-lead refinement (§6–8) **independently** for each accepted component.

---

## 4. Brain mask (when not provided)

**Goal:** Rough intracranial soft-tissue mask, then **convex hull** of that set in voxel space; use hull voxels as brain region.

**Behavioural steps (implementation may differ; results should be equivalent in spirit):**

1. Optional: light **median** filtering to denoise.
2. Threshold **brain tissue** band: roughly **15–60 HU** in standard scale; apply same +1024 offset if `min(ct) >= 0`.
3. Morphology **open** with spherical SE (radius scale with ~2 mm / max voxel size); if resulting mask fraction outside ~0.15–0.30, try **close** then **erode** path (robustness for noisy kernels).
4. Take **largest connected component**, then optionally **slice-wise** largest component per slice (`largestConnCompSliceWise`-style) to reduce neck bridges.
5. **Convex hull** of resulting voxels; fill as binary mask.

**Erosion before metal step:** erode brain mask with spherical SE (~3 mm radius scaled by voxel size) to reduce skull edge inside mask. Wyss DBS Localizer applies the same erosion to a provided `brain_mask` (`02` §7.1).

---

## 5. Metal blob detection and electrode hypothesis

1. Set non-brain voxels to NaN or ignore; threshold: `ct > metal_threshold` (after offset rule).
2. **26-connected** components of metal inside brain.
3. Sort components by voxel count. **Accept** components whose area (voxel count) lies between:
  - **min:** ~cylinder volume 1.27 mm diameter, 40 mm length, with ~20% PV slack:  
   `min_voxels ≈ (1.2 * (1.27/2)^2 * π * 40) / prod(voxel_size)`
  - **max:** ~3× diameter, 80 mm length, 300% PV:  
  `max_voxels ≈ (3 * (1.27/2)^2 * π * 80) / prod(voxel_size)`
4. **Shape filter (PCA elongation):** For each candidate, PCA on voxel centers in **mm** (multiply indices by voxel spacing). Let √eigenvalues ×2 be axis lengths `λ1 ≥ λ2 ≥ λ3`. **Electrode-like** if:
  - `λ1 > 25` mm (elongated along shaft),
  - `λ1 / mean(λ2,λ3) > 10`,
  - `λ3 / (λ2 + ε) < 8` (not too flat/ribbon).  
   Constants **25**, **10**, **8** are part of the behavioural contract (tune only with regression tests).
5. If **no** candidates: if `metal_threshold < 3000`, **retry** recursively with `metal_threshold × 1.2`. If still none, error: no electrode found.
6. **Point cloud per lead:** world coordinates of all voxels in component; parallel **intensity** list (same order).
7. **XML association (optional):** if XML present, for each cloud pick trajectory from plan that minimizes mean squared distance from samples along the planned segment (see Medtronic LPI conversion relative to CT).

### 5a. Head midline for hemisphere keying (Wyss DBS Localizer, normative)

The pipeline keys each accepted lead to a hemisphere by the offset of its distal centre from the head midline (`05` §7, *Sides*). The midline is the mean world *x* (mm) of the **head tissue voxels** of the coregistered CT, where a head tissue voxel has an intensity in [−300, 1800) HU and lies inside the CT's field of view; only every third voxel along each of the three grid axes (voxel indices 0, 3, 6, …) is visited. Without such a voxel there is no midline and the stage fails.

**Field of view.** Resampling the CT onto the anchor grid (`03` §5) fills every grid point outside the moving CT with **0**, which lies inside the tissue band; counted, a tightly cropped or tilted CT would pull the midline towards the filled side. A voxel is therefore **outside the field of view** when it belongs to a run of voxels equal to exactly 0 that reaches either end of its grid row along the first voxel axis (*i*); a row that is 0 throughout is outside entirely. The CT's field of view mapped into the anchor grid is convex, so each row meets it in one interval and the out-of-field voxels of the row are exactly such end runs; zero-valued voxels between non-zero voxels of a row stay head tissue. Implementation: `CtVolume::head_midline_x` (`crates/dbsloc-reconstruct/src/pacer/midline.rs`).

---

## 6. Pass 1 — axial skeleton and high-degree fit

**Assumption:** lead runs roughly **slice-normal** (for typical axial CT, skeleton stacks along **k / z** in **voxel** indices after reorientation; implementations should use **dominant** stack direction if not z — paper assumes axial flow).

1. **Unique plane coordinates:** collect unique **third** coordinate of cloud in mm (tolerance 0 if one point per plane else **0.1 mm** `uniquetol`-style).
2. For each plane with **≥2** points: centroid = **intensity-weighted** mean of in-plane points (weights = voxel HU); track `sumInPlane` for weights.
3. **Filter** skeleton: drop planes where `sumInPlane < median(sumInPlane)/1.5`.
4. **Parametric fit:** build skeleton ordered along stacking direction, then orient it so that its first point (`t = 0`) is the **deeper** end, i.e. the end with the lower world `z` (superior coordinate), whatever axis the planes were stacked along (`pick_stack_axis` may pick x or y for an oblique lead); if `reverse_dir`, reverse that order.
  **Internal fit degree = 8** unless skeleton has fewer than 9 points — then `degree = len-1` with warning.
5. **Parameterization:** For skeleton points S₁…Sₙ, compute cumulative arc length in mm along chords; map to **t** ∈ [0,1] with t₁=0, tᵢ = ∑||ΔS|| / total.
  Design matrix **T** = `[t^d, t^(d-1), …, 1]` (rows = points). **Least squares:** each coordinate column solves `T a =` coordinate column by **QR** of `T` (`a = R⁻¹ Qᵀ b`; normal equations are not used: they square the condition number of the degree-8 Vandermonde matrix). Coefficient matrix **D×3** (rows per degree block — match implementation to §3 output convention).
6. **Total arc length (mm):** for polynomial **p**(`t`) = (x(t),y(t),z(t)),
  `L = ∫_0^1 ||p′(t)|| dt` with `p′` component-wise polynomial derivatives — numerical integration (adaptive) with documented tolerance (e.g. 1e-4 relative).

---

## 7. Pass 2 — optimal oblique resampling (OOR)

**Constants:**

- `XY_RES = 0.1` mm, `Z_RES = 0.025` mm (step along **t**-axis in parameter space, scaled by arc length — see below).
- Grid in **transverse** plane: `X,Y ∈ [-1.5, 1.5]` mm stepped by `XY_RES` (30×30 samples).
- `SND_THRESH = 1500` HU: samples below this **zeroed** for centroid (after NaN→0).
- `lookahead` along **t**: approximately **2 mm** of arc length expressed as `Δt = 2/L` where L is current poly arc length; sampling starts at **t = −lookahead** up to 1 (negative **t** allowed for OOR index grid; **arc-length axis** for negative segments uses signed arc length as in reference behaviour).

**Interpolation:** trilinear (or better) **linear** scatter interpolation from CT **world** points and intensities. Out-of-FOV → NaN; treat as in OOR.

**Per step t_k:**

1. Position **c** = p(`t_k`); tangent **d** = p′(`t_k`) / ||p′||.
2. Build orthonormal basis: **e1** = normalize(cross(d, [0,1,0])), **e2** = normalize(cross(d, e1)).
3. Sample points = **c** + e1·X + e2·Y for all grid pairs.
4. Weights = max(0, I−SND_THRESH) with NaN→0; skeleton point = **intensity-weighted centroid** of samples. **Steps without metal (normative, Wyss DBS Localizer):** when the sum of weights is 0 (no sample above `SND_THRESH`, e.g. before the tip or outside the field of view), the step is **not** dropped: the polynomial point **c** is recorded as the skeleton point and **0** as the step's median intensity (step 5 does not apply to it). Every `t_k` therefore contributes exactly one skeleton point, one median intensity and one `skel_scale_mm` sample, which keeps the profile's arc-length axis complete for §8 and the distal-edge search of §8 step 8a.
5. For a step with a positive sum of weights, record the **median** of its finite samples (NaN samples left out) as the step's intensity along the shaft; **skel_scale_mm**(`k`) = signed arc length from t=0 to t=`t_k` (integral of ||p′||; for `t_k`<0, negative length as in reference).

**Refit:** Apply **fitParamPolyToSkeleton** to OOR skeleton with degree **8** for intermediate poly. Run OOR again on that poly to get final skeleton + `med_intensity` + `skel_scale_mm` for peak detection.

---

## 8. Intensity-based contact detection

1. **Band-limit:** `filter_width = round(0.25 mm / Z_RES) + 1` samples; apply **zero-phase** smoothing (e.g. forward-back moving average) to `med_intensity`.
2. **Search window:** only `skel_scale_mm ≤ 20` mm (tip region).
3. **Peaks:** on smoothed profile, find local maxima in **s** (mm) with:
  - min distance **1.4** mm,
  - height ≥ **1.1 × nanmean(profile)**,
  - prominence ≥ **0.01 × nanmean(profile)**.
4. **Wave centres (optional method):** from first 4 peaks, build threshold; label samples above/below; centre of mass of each connected “high” run gives **wave centres**.
5. **Contact area (fallback):** threshold at **mean** intensity in window; `bwlabel` on binary runs along **s**; second run’s centre = **contact_area_center**; width = extent in mm. If multiple runs, third run may be x-ray marker.
6. **Electrode typing:** for each prototype **k**, `diffs_mm` = expected spacing vector between adjacent contacts. Compare **diff(peak_positions)** to each prototype:
  - score `dist_k = || diff(peaks) − diffs_mm_k ||` (L2); pick argmin; RMS for quality.
  - If all fail (Inf), unknown type.
  - **Tie = ambiguous:** when two prototypes share the minimal `dist_k` exactly (identical `diffs_mm`, e.g. Medtronic 3389 and Boston Vercise Directional), the spacing cannot tell them apart: the type is **unknown** (score and RMS `Inf`), not the first prototype in table order.
7. **RMS check:** if RMS > **0.3** mm, fall back to model-based (ignore detected peaks) or `contact_area_center` path (same as manual type / low-SNR path).
8. **Zero calibration along shaft:**
  - **With good peaks + type:** `t0 = inv_arc_length( first_peak_mm − zero_to_first_peak_mm )` where `inv_arc_length` inverts `s`↔`t` using the **second-pass** polynomial (before final degree reduction).  
  - **Area-based / manual type:** `t0 = inv_arc_length( contact_area_center − mean(ring_contact_centers_mm) )` (behavioural; matches fallback when individual contacts not resolved).
9. **Final polynomial:** resample `p(t)` on `t ∈ [t0, 1]` at spacing ~`XY_RES `along **arc in mm**; refit with` **final_poly_degree`**. If degree = 1, clip evaluation window to first 20 mm for stability then re-extend. Re-parameterize [0,1] after fit so that tip remains consistent.

8a. **Distal-edge calibration (Wyss DBS Localizer, normative for the in-tree implementation):** on real post-op CT the contacts bloom into one continuous plateau, so the contact-area run of step 5 is a later bump of that plateau rather than the contact band. Measured 2026-10-04 on the Lead-Tutor CTs, the area path put `head` 6.6–12 mm up the shaft. Wyss DBS Localizer therefore calibrates the apex on the **distal metal edge** whenever the smoothed profile shows one: within `skel_scale_mm ≤ 20` (the profile holds one sample per OOR step, the metal-free steps before the tip with intensity 0, §7 step 4), background = 20th percentile, plateau = median of the samples above the window mid-range, threshold = half-way; the edge is the first upward crossing that stays above threshold for ≥ 1 mm, linearly interpolated, and the profile must start below the threshold. `apex_s = edge + distal_bloom_mm − d_metal`, where `d_metal` is 0 when the tip is a contact and the tip length otherwise (`specs/09`), and `distal_bloom_mm` = **0.5** (median along-shaft head error against the 10 expert sides 0.08 mm at that value; 0 for a phantom painted without blooming). Levels are placed on the second-pass polynomial at `apex_s + apex_to_level_mm[k]`. With **no edge** (the skeleton starts inside the metal) the lead is refused with `NoDistalEdge` instead of falling back to step 8's area path. The apex may lie at most 0.5 mm before the skeleton start and every level centre must fall within the skeleton's arc length, otherwise the lead is refused with `ApexBeforeSkeleton` / `LevelsPastSkeleton` (the arc-length inverse clamps, so out-of-range levels would otherwise collapse onto an end point). Errors are per lead: `pacer_leads` returns each accepted component with its own result and distal centre.

`inv_arc_length(Δs)`: find **t** such that `∫_0^t ||p′(u)|| du = Δs` (use 1D root find on [0,1] with `poly_arc_length`).

---

## 9. Data-driven electrode geometry table

Implementation must ship **at least** the Medtronic **3389**, **3387**, and **Boston Vercise Directional** prototypes: **diameter**, **ring_contact_centers_mm** (monotonic from proximal zero), **zero_to_first_peak_mm**, **tip_to_first_peak_mm**, **diffs_mm**, **n_ring_contacts** — values taken from **manufacturer IFU** or the Husch 2017 supplement, **not** copied from any AGPL `.mat` file. Until validated, use explicit `UNSPECIFIED` in tests and mark `#[ignore]`; wire constants behind a single `electrode_catalog` module for audit.

**Fallback width rule (when type unknown, area only):** if `contact_area_width < 10.5` mm → assume 3389-class spacing; else 3387-class (documented in paper as heuristic).

---

## 10. Errors


| Code               | When                                                                  |
| ------------------ | --------------------------------------------------------------------- |
| `NoElectrode`      | No component passes elongation + volume after retries.                |
| `ObliqueCT`        | Cannot establish stacked planes for skeleton (non-axial acquisition). |
| `EmptySkeleton`    | Degenerate skeleton after filtering.                                  |
| `UnknownElectrode` | User string not in catalog.                                           |
| `Interpolation`    | OOR all-NaN for extended range.                                       |
| `NoDistalEdge`     | §8 step 8a found no distal metal edge (skeleton starts inside metal). |
| `ApexBeforeSkeleton` | §8 step 8a apex or first level before the skeleton start.           |
| `LevelsPastSkeleton` | §8 step 8a most-proximal level beyond the traced skeleton.          |


---

## 11. Acceptance tests (non-exhaustive)

1. **Synthetic line cloud:** voxels on straight line; recovered polynomial end-to-end deviation < small tolerance at contact region.
2. **Polynomial:** arc length of **unit speed** line segment 10 mm → `L ≈ 10` mm; `inv_arc_length(5) ≈ 0.5` in normalized parameter for straight case.
3. **PCA filter:** synthetic elongated ellipsoid (λ1/λ2 large) accepted; oblate rejected.
4. **Regression:** optional golden NIfTI (no AGPL assets) for pixel-approximate match when pipeline complete.

---

## 12. Implementation notes (Rust, `dbsloc-reconstruct`)

- **Phase 1 (allowed now):** §6 math utilities — parametric R³ poly, `fit_param_poly_to_skeleton`, `poly_arc_length3`, `inv_poly_arc_length3`, `polyval3`; unit tests from §11.1–11.2.
- **Phase 2:** §4–5 (mask, connected components, PCA) on ndarray volumes.
- **Phase 3:** §7 OOR with trilinear grid sampling.
- **Phase 4:** §8 peak finding + catalog + final refit.
- NIfTI I/O stays outside or uses existing `fastwarp-core` / workspace conventions.

**Independent-implementation rule:** implementers of Phases 2+ must not read PaCER or Lead-DBS source — **only** this file + Husch 2017 + open datasheets.