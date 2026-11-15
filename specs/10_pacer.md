# Spec 10 — PaCER CT Electrode Reconstruction

**Status:** Implemented: lead detection in a post-operative CT, curved trajectory models, PaCER's own contact detection and lead typing, and contact placement from the distal metal edge; nothing further is planned.
**Depends on:** `02` §7.1 (template brain mask), `05` §7 (pipeline interface: when PaCER runs, side keys, reconstruction file, stage errors), `09` (lead geometry: fiducials §2.2, level depths §10.1).

PaCER (Husch et al., §12) reconstructs deep brain stimulation (DBS) leads from a post-operative CT alone: it finds the metal artifact of each lead, fits a curved centreline in three passes and locates the contacts on a one-dimensional intensity profile along that centreline. This specification describes the method as this project implements it. Every numeric value is listed in the parameter table of §2 with its source: *paper §n* when the paper states it, *implementation* when it is a parameter of this implementation; the sections refer to the parameters by their names in italics. `crates/dbsloc-pacer` is an independent implementation written from this specification and the paper. It is the optional AGPL-3.0-only crate built behind the default-on cargo feature `pacer` of `dbsloc-pipeline` (`docs/LICENSING.md`, *PaCER and the AGPL*); without the feature, `recon_method = pacer` fails with an error (`05` §7).

---

## 1. Scope and conventions

- **In scope.** One CT volume in Hounsfield units (HU); every metal component that looks like a lead; for each, curved centreline models, the level centres and the `head` / `tail` / `x` / `y` fiducials in the CT's world frame. The pipeline accepts one or two leads (`05` §7).
- **Out of scope.** Segment rotation of directional leads (paper §4.4; orientation is `06`), post-operative MRI (paper §4.3.1), comparison with a surgical plan (paper §3.4.2), and the viewers and stimulation model of the original toolbox.
- **Frames.** World coordinates are the CT's scanner millimetres. Voxel centres lie at integer voxel indices, mapped to world by the CT affine (NIfTI convention). A provided brain mask lies on the CT's voxel grid.
- **Arc length.** `s` is the signed arc length in mm along a trajectory model from its parameter 0 (§6.1); `s < 0` lies beyond parameter 0 on the deep side.
- **Code.** File names without a directory are in `crates/dbsloc-pacer/src/`.

## 2. Inputs and parameters

The inputs are held by `PacerConfig` (`pipeline.rs`):

| Input | Default |
| ----- | ------- |
| CT volume with its affine (`CtVolume`, `volume.rs`) | — |
| Voxel size | 1 mm isotropic; the pipeline sets it from the column norms of the affine |
| Brain mask on the CT grid | none: computed (§4) |
| Treat the whole volume as brain (phantoms) | off |
| Reverse the direction of the first-pass skeleton (§6.2) | off |
| Locate the contact area instead of individual contacts (paper §4.1) | off |
| Lead type for PaCER's own typing (§8.4) | none |
| *Metal threshold*, *final degree*, *distal bloom* | as in the parameter table |

**Offset-positive CT.** When the smallest CT value is ≥ 0, the volume is taken to store HU plus the *CT offset*: the *metal threshold* (§5) and the *tissue band* (§4) are raised by it. The *metal weight threshold* (§7) and the *head-tissue band* (§5a) apply to the stored values unchanged.

**Pipeline.** `crates/dbsloc-pipeline/src/ct_recon.rs`, `fn pacer_reco`, runs PaCER on the coregistered CT with these defaults, the template brain mask when one lies on the CT grid (`02` §7.1), the voxel size of the affine and the subject's electrode model pinned (§8a step 3). It replaces the *distal bloom* with the request's `distal_bloom_mm` when that is set, which only the Python research binding does (`05` §7).

**Parameters.**

| Parameter | Value | Source | Section |
| --------- | ----- | ------ | ------- |
| CT offset | 1024 | implementation | §2 |
| Tissue band | 15 to 60 HU, both excluded | implementation | §4 |
| Majority filter | 3 × 3 × 3; set when more than 13 of the 27 voxels are set | implementation | §4 |
| Opening ball | 2 mm | implementation | §4 |
| Accepted coverage | 15 % to 30 % of the volume | implementation | §4 |
| Hull points | all surface voxels when fewer than 10 000, else an evenly strided subset of 5000 to 10 000 | implementation | §4 |
| Erosion ball | 3 mm | implementation | §4 |
| Metal threshold | 800 HU | implementation | §5 |
| Lead volume range | 60.8 to 304 mm³ | implementation | §5 |
| Minimum length | 25 mm | implementation | §5 |
| Elongation | 10 | implementation | §5 |
| Flatness | 8 | implementation | §5 |
| Threshold retry | × 1.2 per attempt while the threshold is below 3000 HU; at most 6 attempts | implementation | §5 |
| Head-tissue band | −300 HU (included) to 1800 HU (excluded) | implementation | §5a |
| Midline grid | every third voxel along each axis (indices 0, 3, 6, …) | implementation | §5a |
| Plane bin width | 0.1 mm | implementation | §6.2 |
| Plane weight filter | median plane weight / 1.5 | implementation | §6.2 |
| Internal degree | 8 | paper §4.3.2 | §6.2, §7 |
| Resampling step | 0.025 mm | paper §2.4 | §7 |
| In-plane spacing | 0.1 mm | paper §2.4 | §7 |
| Plane half-width | 1.5 mm (31 × 31 points) | implementation | §7 |
| Extension | 2 mm beyond the deep end, at most half the parameter range | implementation | §7 |
| Metal weight threshold | 1500 | implementation | §7 |
| Smoothing window | 11 samples (about 0.25 mm), applied twice | implementation | §8.1 |
| Tip window | `s ≤ 20 mm` | implementation | §8 |
| Individual-contact limit | largest voxel dimension 1 mm | implementation | §8.2 |
| Peak height | 1.1 × profile mean | implementation | §8.3 |
| Peak prominence | 1 % of the profile mean | implementation | §8.3 |
| Peak separation | 1.4 mm | implementation | §8.3 |
| Peak spacing tolerance | 0.3 mm root mean square | implementation | §8.4 |
| Width threshold | 10.5 mm, the Medtronic 3387 contact span (§9) | implementation | §8.5 |
| Final degree | 3 | paper §4.3.2 | §8.6 |
| Final-model samples | 61, evenly spaced in parameter | implementation | §8.6 |
| Edge background | 20th percentile of the tip window | implementation | §8a |
| Edge hold | 1 mm | implementation | §8a |
| Distal bloom | 0.5 mm | implementation | §8a |
| Apex tolerance | 0.5 mm before parameter 0 | implementation | §8a |
| Distal-centre sampling | 9 points evenly spaced over the distal 20 mm | implementation | §3 |

## 3. Outputs

`pacer_leads` (`fiducials.rs`) returns one `PacerLead` per accepted metal component, in the order of §5. Each holds its **distal centre**, the mean of the *distal-centre sampling* points on the second-pass model (§7; over its whole length when shorter), or the centroid of its metal voxels when the lead could not be refined, which keys the lead to a hemisphere (`05` §7); and either the placed lead (`PacerSide`) or the error that prevented it (§10). `pacer_sides` returns the placed leads only and fails on the first lead error.

`PacerSide` holds `head`, `tail`, `x` and `y` (§8a step 5), one point per level (distal first), the catalogue name of the geometry used, the shaft length (arc length of the final model, §8.6), the smoothed profile with its arc positions (§8.1) and the arc position of the tip apex (§8a).

The result of a lead before placement (`PacerElectrode`, `pipeline.rs`) holds the final model, the prototype typed or given (§9), whether detected peaks were used (§8.4), the smoothed profile with its arc positions and the second-pass model.

The pipeline writes the placed fiducials and level centres, not the polynomial models: the native block of the reconstruction file holds the markers, `coords_mm` with one row per level and the straight 50-point trajectory built from `head` and `tail` (`05` §5, §7).

## 4. Brain mask

The paper restricts every step to a brain mask computed from the CT itself and relaxed to its convex hull (paper §2.1). `mask.rs` computes it as follows. A ball of *r* mm is a ball in voxel index space whose radius is *r* divided by the largest voxel dimension.

1. **Tissue band.** Voxels inside the *tissue band*.
2. **Denoise.** The *majority filter*, with the volume edge replicated.
3. **Open** with the *opening ball* (radius at least 1 voxel). When the result covers a fraction of the volume outside the *accepted coverage*, the band mask is instead closed with the same ball and then eroded with it (radius at least 0.5 voxel).
4. **Largest component** (26-connected).
5. **Convex hull.** The convex hull in world mm of the component's surface voxels (*hull points*) replaces the component: the mask becomes every voxel whose centre lies inside the hull. A component with fewer than 8 surface voxels, or with a degenerate hull, is kept as it is.
6. **Erode** with the *erosion ball* (radius at least 1 voxel), so that the skull edge stays outside.

A provided mask replaces steps 1–5 and receives the same erosion (`02` §7.1); one whose shape differs from the CT's fails the run (`BrainMaskGrid`, §10). When the whole volume is treated as brain, no mask is built.

## 5. Metal components

The paper detects the metal components of the leads inside the brain mask (paper §2.1). `metal.rs` does so as follows.

1. **Threshold.** Brain-mask voxels with a value above the *metal threshold*.
2. **Components.** 26-connected components of those voxels.
3. **Size.** A component is a candidate when its volume (voxel count × voxel volume) lies in the *lead volume range*: 1.2 times a cylinder 1.27 mm wide and 40 mm long up to 3 times a cylinder 1.27 mm wide and 80 mm long.
4. **Shape.** Principal component analysis of the candidate's voxel centres in world mm gives axis lengths `a1 ≥ a2 ≥ a3`, each twice the square root of an eigenvalue of the covariance matrix. The candidate is a lead when `a1` exceeds the *minimum length*, `a1 / ((a2 + a3) / 2)` exceeds the *elongation* and `a3 / a2` stays below the *flatness*.
   Implementation status: the third condition always holds because the axes are sorted (`a3 ≤ a2`), so it rejects nothing.
5. **Retry.** When no candidate passes, the threshold is raised and the search repeated within the limits of the *threshold retry*; then the run fails with `NoElectrode`.
6. **Result.** Every accepted component, largest first, as the world positions of its voxel centres with their CT values.

### 5a. Head midline (normative)

The pipeline keys each accepted lead to a hemisphere by the offset of its distal centre (§3) from the head midline (`05` §7). The midline is the mean world *x* (mm) of the **head-tissue voxels** of the CT, where a head-tissue voxel has a value in the *head-tissue band* and lies inside the CT's field of view; only the voxels of the *midline grid* are visited. Without such a voxel there is no midline and the stage fails (`05` §7). Implementation: `CtVolume::head_midline_x` (`midline.rs`).

**Field of view.** Resampling the CT onto the anchor grid (`03`) fills every grid point outside the moving CT with **0**, which lies inside the tissue band; counted, a tightly cropped or tilted CT would pull the midline towards the filled side. A voxel is therefore **outside the field of view** when it belongs to a run of voxels equal to exactly 0 that reaches either end of its grid row along the first voxel axis (*i*); a row that is 0 throughout is outside entirely. The CT's field of view mapped into the anchor grid is convex, so each row meets it in one interval and the out-of-field voxels of the row are exactly such end runs; zero-valued voxels between non-zero voxels of a row stay head tissue.

## 6. Trajectory models and first pass

### 6.1 Polynomial model (paper §2.2, §2.3)

A trajectory model is a curve `p(t)`, `t ∈ [0, 1]`, whose x, y and z coordinates are independent polynomials of one degree (paper §4.3.2) (`poly3.rs`).

- **Fit.** The ordered skeleton points get parameters proportional to their cumulative chord length (first point 0, last point 1). Each coordinate is fitted by least squares through a QR decomposition of the matrix of powers of `t`, not through the normal equations, which would square the condition number of the degree-8 problem. The fit fails with fewer points than coefficients or a total chord length of zero.
- **Arc length.** The arc length between two parameters is the integral of the speed `‖p′(t)‖` (paper §2.3), negative when the end parameter precedes the start.
- **Inverse arc length.** The parameter at which the arc length from a start parameter reaches a given `s` (paper §2.3). It clamps: `s ≤ 0` gives the start, an `s` beyond the end gives the end.

Both are evaluated numerically, accurate to well below 0.01 mm (implementation).

### 6.2 First-pass skeleton (paper §2.1, §2.2)

The paper builds the first skeleton from axial slices and notes that a CT tilted by more than about 45° would need another slice direction (paper §4.3.1). This implementation chooses the direction itself (`skeleton_pass1.rs`).

1. **Plane direction.** The world axis (x, y or z) along which the component's coordinates, binned at the *plane bin width*, take the most distinct values.
2. **Plane centroids.** Voxels are grouped into planes by that binned coordinate. Each plane with at least two voxels gives one skeleton point: the mean of its voxel positions weighted by their CT values (negative values weigh 0). Fewer than two planes fail the lead with `ObliqueCt`.
3. **Filter.** Planes whose total weight is below the *plane weight filter* are dropped; fewer than two remaining fail the lead with `EmptySkeleton`.
4. **Order.** The points are ordered along the plane axis and then oriented so that the first point (`t = 0`) is the deeper end, the one with the lower world *z*, whatever the plane axis; the reverse option reverses that order.
5. **Fit.** The *internal degree*, or one less than the number of points when there are fewer. The result is the first-pass model.

## 7. Oblique resampling and second pass (paper §2.4)

One resampling pass along a model (`oor.rs`) visits a sequence of steps:

1. **Steps.** Parameters from the *extension* beyond the deep end up to 1, in equal parameter increments of about one *resampling step* of arc. The polynomial is extrapolated beyond its skeleton there, so that the profile starts before the tip.
2. **Plane.** At each step a square grid with the *in-plane spacing* and the *plane half-width*, perpendicular to the model's tangent and centred on the model point. Its axes are an orthonormal basis of that plane; their rotation about the tangent is arbitrary.
3. **Sampling.** Trilinear interpolation of the CT at each grid point. A point outside the voxel grid has no value.
4. **Skeleton point and profile value.** The skeleton point is the mean of the grid positions weighted by `max(0, value − metal weight threshold)`, points without value weighing 0 (intensity-weighted mean, paper §2.4). The profile value is the median of the values sampled at the step, points without value left out (paper §2.5). **Steps without metal (normative):** when every weight is 0 (before the tip, outside the field of view), the step keeps the model point as its skeleton point and 0 as its profile value. Every step therefore contributes exactly one skeleton point, one profile value and one arc position, which keeps the arc axis of the profile complete for §8.
5. **Arc position.** The signed arc length `s` from parameter 0 to the step, negative before it.

**Second pass.** A first resampling along the first-pass model gives a skeleton; refitted at the *internal degree* (one less than the number of points when shorter) it becomes the second-pass model (paper §2.4). When that fit fails, the first-pass model is kept. A second resampling along the second-pass model gives the intensity profile and its arc positions; its skeleton points are not used. A first resampling with fewer than four steps fails the lead with `Interpolation`.

## 8. Contact detection and zero point (paper §2.5)

PaCER's own detection (§8.1–§8.6) yields the smoothed profile, the lead type and the final model. The smoothed profile is what §8a measures the distal edge on; the type selects the placement geometry when no electrode model is pinned (§8a step 3; the pipeline always pins one); the final model gives only the reported shaft length. The contacts are placed by §8a.

### 8.1 Intensity profile

The profile of §7 is smoothed with a centred moving average over the *smoothing window*, shortened at the ends of the profile and applied twice, which leaves peaks in place (zero-phase filtering, paper §2.5) (`peaks.rs`). §8.3–§8.5 and §8a work on the smoothed profile within the *tip window*.

### 8.2 Individual contacts or contact area

Individual contacts are searched unless the contact area is preferred (§2), the largest voxel dimension exceeds the *individual-contact limit*, or a lead type is given (`pipeline.rs`). The paper resolves individual contacts on thin-slice CTs and falls back to the contact area on thicker slices (paper §3.2, §3.3, §4.2), and lets the user choose the contact area (paper §4.1). `pacer_leads` (`fiducials.rs`) hands the pinned model over as lead type whenever the prototype table recognises its name (§9), so in the pipeline Medtronic 3389 and 3387 and every Boston Scientific lead take the contact-area path.

### 8.3 Peaks

A peak is a sample of the tip window larger than both neighbours, at least the *peak height*, with at least the *peak prominence*; the profile mean, taken over the whole profile, must be positive. Scanning from distal to proximal, a peak closer than the *peak separation* to an already kept peak is dropped (`peaks.rs`).
Implementation status: the prominence is measured only against the lower of the two neighbouring samples (0.025 mm apart), not as the height above the surrounding minima, so it can reject real maxima on a smoothed profile.

### 8.4 Lead type from the peak spacing (paper §2.5.2)

With at least four peaks, the three spacings between the first four are compared with the three level pitches of each prototype (§9) by Euclidean distance, and the nearest prototype is the type (`catalog.rs`). **Tie:** when two prototypes share the smallest distance, as Medtronic 3389 and Boston Scientific Vercise Directed always do (same pitches), the type is unknown, not the first prototype in table order. The detected peaks are used for the zero point when their spacing error stays within the *peak spacing tolerance*. The paper also matches the peaks of the Boston Scientific orientation marker; this implementation does not, so peak typing returns Medtronic 3387 or no type.

**Precedence** (`pipeline.rs`). A given lead type wins; otherwise the contact-area path uses the width rule of §8.5; otherwise the peak type; otherwise the width rule.

### 8.5 Contact area (paper §3.2)

Tip-window samples at or above the mean of the tip window form runs; the **second** run is the contact area, its centre the midpoint and its width the distance between the arc positions of its first and last samples (`peaks.rs`). With fewer than two runs there is no contact area. **Width rule:** a width of at least the *width threshold* gives the Medtronic 3387; a smaller width or no contact area gives the Medtronic 3389.

### 8.6 PaCER zero point and final model (paper §2.5.1, §4.3.2)

The paper defines the zero point of the final model as the distal edge of the first contact, derived from the detected contacts, and refits the model after contact detection. This implementation starts the final model at the estimated tip apex `s0`:

- with detected peaks, `s0` = first peak − the prototype's apex-to-first-level distance;
- otherwise, `s0` = contact-area centre − the mean of the prototype's four apex-to-level distances, and `s0` = 0 without a contact area;
- a negative `s0` becomes 0.

The final model is fitted at the *final degree* to the *final-model samples* of the second-pass model, taken from the inverse arc length of `s0` to parameter 1 (`pipeline.rs`). A failed fit fails the lead with `EmptySkeleton`.

### 8a. Distal-edge zero point and level placement (normative)

On real post-operative CT the contacts bloom into one continuous plateau, so neither individual peaks nor the contact-area run of §8.5 locate them: on the Lead-Tutor CTs the contact-area path put `head` 6.6–12 mm too far up the shaft. The contacts are therefore placed from the **distal metal edge** of the smoothed profile (§8.1), which is the paper's zero point (the distal edge of the first contact, paper §2.5.1) measured directly (`fiducials.rs`).

1. **Edge.** Within the tip window: background = the *edge background*; plateau = median of the samples above the window's mid-range (half-way between background and maximum); threshold = half-way between background and plateau. The edge is the first upward crossing of the threshold after which the profile stays at or above it for the *edge hold*, interpolated linearly between the samples on either side of the crossing. There is no edge when the window has no contrast, when its first sample is at or above the threshold (the skeleton starts inside the metal) or when no crossing holds; the lead is then refused with `NoDistalEdge`, never placed from §8.6.
2. **Apex.** `apex_s = edge + distal bloom − d`, where `d` is 0 when the tip is a contact and the tip length otherwise (`09` §2, §10.1). The *distal bloom* is how far the half-maximum edge lies beyond the real end of the metal; at its value the median along-shaft error of `head` against the Lead-Tutor reference reconstructions is 0.08 mm. A phantom painted without blooming wants 0.
3. **Geometry.** The electrode model pinned by the caller, else the catalogue entry named like the prototype PaCER typed (`09`); a name the catalogue lacks fails with `UnknownElectrode`. A pinned model is also handed to PaCER's own typing when the prototype table recognises it (§8.2, §9), so the two cannot disagree.
4. **Levels.** Level *k* lies on the second-pass model at arc length `apex_s + z_1 + z(k)` from parameter 0, `z_1 + z(k)` being the level's distance from the apex (`09` §10.1; `ElectrodeGeometry::apex_to_level_mm`). The apex may lie at most the *apex tolerance* before parameter 0 and the first level not before it (`ApexBeforeSkeleton`); the last level must lie within the arc length of the second-pass model over [0, 1] (`LevelsPastSkeleton`). The inverse arc length clamps, so without these checks a level outside the model would collapse onto an end point. A model with fewer than two levels fails with `EmptySkeleton`.
5. **Fiducials.** `head` is the first level and `tail` the model's tail fiducial level (`09` §2.2). `x` and `y` lie one lead radius from `head` in the plane perpendicular to `head → tail`: `x` along the component of world +z perpendicular to the lead (of world +x when the lead is within about 26° of the z axis; implementation), `y` along the cross product of the unit vector from `head` to `tail` with `x` (`crates/dbsloc-reconstruct/src/ring.rs`, `ring_in_plane`). This roll is arbitrary until orientation (`06`) sets it.

Errors are per lead: a lead that cannot be placed does not discard the others (`pacer_leads`); the pipeline decides what follows (`05` §7).

## 9. Prototype table (paper §2.5.2)

| Prototype | Level pitches (mm) | Level centres from the tip apex (mm) | Diameter (mm) |
| --------- | ------------------ | ------------------------------------ | ------------- |
| Medtronic 3389 | 2.0, 2.0, 2.0 | 2.25, 4.25, 6.25, 8.25 | 1.27 |
| Medtronic 3387 | 3.0, 3.0, 3.0 | 2.25, 5.25, 8.25, 11.25 | 1.27 |
| Boston Scientific Vercise Directed | 2.0, 2.0, 2.0 | 0.75, 2.75, 4.75, 6.75 | 1.3 |

Code: `catalog.rs`. Pitches are centre to centre. The values follow the manufacturers' dimensions and must agree with the shared catalogue (`09` §7.1, §7.3), which the test `prototypes_agree_with_the_shared_catalogue` enforces. The table serves only PaCER's own typing and §8.5–§8.6; contacts are placed from the shared catalogue (§8a, `09` §9).

- **Name matching** ignores case: a name containing `3389`, `3387`, or `verc` or `boston` selects that prototype, so every Boston Scientific model maps to the Vercise Directed prototype; any other name fails with `UnknownElectrode`.
- **Contact span**: last minus first level centre plus the 1.5 mm contact length, 7.5 mm for the 3389 and 10.5 mm for the 3387. The *width threshold* (§8.5) is computed from the 3387 row.

## 10. Errors

`PacerError` (`errors.rs`). *Run* errors end `pacer_leads`; *lead* errors are reported per lead (§3).

| Error | Message | When | Scope |
| ----- | ------- | ---- | ----- |
| `NoElectrode` | `no electrode artifact found after threshold retries` | §5 step 5 | run |
| `BrainMaskGrid` | `the brain mask has shape (<k>, <j>, <i>) but the CT has shape (<k>, <j>, <i>) (k, j, i)` | §4 | run |
| `UnknownElectrode` | `unknown electrode type string: <name>` | pinned model not in the catalogue (run); typed prototype not in the catalogue (lead, §8a step 3); configured lead type the prototype table does not know, name lower-cased (lead) | run, lead |
| `ObliqueCt` | `CT not usable as stacked planes for skeleton (oblique or degenerate)` | §6.2 step 2 | lead |
| `EmptySkeleton` | `skeleton empty after filtering` | §6.2 steps 3 and 5; §8.6; §8a step 4 | lead |
| `Interpolation` | `interpolation / OOR produced no valid samples` | §7 | lead |
| `NoDistalEdge` | `no distal metal edge in the first 20 mm of the intensity profile; the tip lies before the traced skeleton, so the contacts cannot be placed` | §8a step 1 | lead |
| `ApexBeforeSkeleton` | `calibrated tip apex lies <mm> mm along the skeleton, before its start; the levels cannot be placed on it` | §8a step 4; `<mm>` is the apex position, or the first level's when that check fails | lead |
| `LevelsPastSkeleton` | `the most-proximal level needs <mm> mm of skeleton but only <mm> mm was traced` | §8a step 4 | lead |

`Hull` (`convex hull build failed: <why>`) and `DegeneratePca` (`min eigenvalue in PCA: degenerate point set`) are declared but no code path produces them. The pipeline reports a run error as `PaCER: <message>` and collects lead errors into `PaCER could not place <n> of <m> lead(s): …` (`05` §7).

## 11. Tests

`cargo test -p dbsloc-pacer` runs the unit tests of `crates/dbsloc-pacer/src/` and `crates/dbsloc-pacer/tests/pacer_phantom.rs`:

- `poly3.rs`: straight-line fit, arc length and inverse arc length; a degree-8 fit stays within 0.05 mm of a smooth 50 mm curve; coincident points do not fit.
- `volume.rs`: a singular affine gives no samples; the inverse affine recovers indices of an oblique affine to 10⁻⁹.
- `mask.rs`: the parallel morphology equals a serial reference; the row-wise hull fill equals a per-voxel hull test on three affines, one oblique.
- `metal.rs`: cloud points are voxel centres; the shape test accepts a thin rod and rejects an isotropic blob; a rod is found as a lead.
- `midline.rs` (§5a): a cropped field of view does not move the midline; zero runs at row ends are outside; interior zeros count; without tissue there is no midline.
- `skeleton_pass1.rs`: parameter 0 lies at the deep end for oblique leads whatever the plane axis; the reverse option flips it.
- `oor.rs`: steps without metal keep the model point and profile value 0; the profile starts at `s` = −2 mm.
- `catalog.rs`: pitches, first-level distances, contact spans 7.5 / 10.5 mm, the 3387 recognised from ideal peaks, the 3389 / Vercise Directed tie unknown, name matching, agreement with the shared catalogue.
- `fiducials.rs`: levels at catalogue depths including the tip-contact offset; `x` / `y` at the lead radius; NIfTI index order preserved; the distal edge found within 0.1 mm on a bloomed profile and absent when the profile starts in metal; `ApexBeforeSkeleton` and `LevelsPastSkeleton`; `head` placed from the edge; `tail` on the tail fiducial level of an 8-level lead; an unknown pinned model refused.
- `crates/dbsloc-pacer/tests/pacer_phantom.rs`: a synthetic CT (0.5 mm voxels) with one 55 mm lead: Medtronic 3387 levels within 0.8 mm (worst measured 0.62 mm) and a head–tail span within 0.3 mm of the fiducial span, the span also for a 3389; an empty volume fails; a provided mask replaces the computed one, a mask without the lead gives `NoElectrode`, a mask on another grid `BrainMaskGrid`.

Pipeline tests (side keys, placement, refusal without the feature) are listed in `05` §11.

## 12. References

- Husch A, Petersen MV, Gemmar P, Goncalves J, Hertel F. PaCER — A fully automated method for electrode trajectory and contact reconstruction in deep brain stimulation. *NeuroImage: Clinical* 17 (2018), 80–89, published online 2017. doi:10.1016/j.nicl.2017.10.004. "Paper §n" in this specification refers to the sections of this article.
