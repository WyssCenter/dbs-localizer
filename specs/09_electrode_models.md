# Specification 09 — Electrode Models

**Phase:** A (Analysis) → B (Specification)
**Status:** Phase A complete — all 73 models catalogued. Phase B spec complete.
**Source analysed:** `<leaddbs>/templates/electrode_models/ea_resolve_elspec.m` (a local Lead-DBS checkout) plus all `ea_elspec_*.m` files and mesh subdirectories.

---

## 1. Purpose and Scope

This document specifies every electrode model supported by Lead-DBS, the `elspec` struct that describes them, the `ea_resolve_elspec` lookup mechanism, and the 3-D mesh/template files that accompany each model. It is the authoritative reference for implementing `dbsloc.electrodes`.

Scope:

- 73 clinical and research electrode models (DBS, SEEG, ECoG, rat)
- `elspec` struct field definitions
- Contact geometry conventions (tip, insulator, segmented contacts)
- Marker (rotation-indicator) geometry
- Electrode-level struct (`electrode`) produced by `ea_elspec_*.m` builders
- DiODe directional-orientation support matrix
- 3-D mesh file catalogue

Out of scope: reconstruction algorithm, VAT modelling, visualisation rendering.

---

## 2. The `elspec` Struct — Field Reference

Every electrode is described by a scalar MATLAB struct `elspec`. The following table lists every field that appears across all models, its type, and its meaning. All linear dimensions are in **millimetres**.


| Field              | Type             | Description                                                                                                                                                             |
| ------------------ | ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `elmodel`          | string           | Human-readable display name (e.g. `'Medtronic 3389'`) — canonical lookup key                                                                                            |
| `matfname`         | string           | Base filename (no extension) of the precomputed `.mat` mesh (e.g. `'medtronic_3389'`)                                                                                   |
| `numContacts`      | int              | Number of individual contact elements per lead (not per side)                                                                                                           |
| `contact_length`   | float            | Axial height of one contact cylinder (mm)                                                                                                                               |
| `contact_diameter` | float            | Outer diameter of one contact (= insulator diameter for cylindrical contacts) (mm)                                                                                      |
| `contact_color`    | float ∈ [0,1]    | Greyscale rendering value for contacts (0.3 in all clinical models)                                                                                                     |
| `contact_spacing`  | float or float[] | Edge-to-edge gap between adjacent contacts (mm). Scalar for uniform spacing; 1-D array of length `numContacts−1` for non-uniform spacing                                |
| `lead_diameter`    | float            | Outer diameter of the insulating shaft (mm)                                                                                                                             |
| `lead_color`       | float ∈ [0,1]    | Greyscale rendering value for the shaft (0.7 in all models)                                                                                                             |
| `tip_diameter`     | float            | Outer diameter at the tip/cap (mm); equals `lead_diameter` for blunt tips                                                                                               |
| `tip_length`       | float            | Axial length of the tip section below/beyond the first contact (mm)                                                                                                     |
| `tip_color`        | float ∈ [0,1]    | Greyscale value for the tip; 0.3 if `tipiscontact=1`, else 0.7                                                                                                          |
| `tipiscontact`     | 0 or 1           | 1 → the tip itself is the first (most-distal) contact; 0 → tip is an insulating cap                                                                                     |
| `isdirected`       | 0 or 1           | 1 → lead has directional (segmented) contacts requiring orientation detection                                                                                           |
| `forstimulation`   | 0 or 1           | 1 → lead is intended for stimulation (all current models = 1)                                                                                                           |
| `contactnames`     | cell string[]    | Length `2 × numContacts`. Names for right-side contacts followed by left-side contacts. Used for display and channel labelling                                          |
| `etagenames`       | {cell,cell}      | `etagenames{1}` = right-side group names; `etagenames{2}` = left-side group names. One name per electrode *level* (ring or segmented group), not per segment            |
| `etageidx`         | cell of int[]    | `etageidx{k}` = 1-based index vector into the per-side contact list for level `k`. Length equals number of stimulation levels                                           |
| `markerpos`        | float            | (directed leads only) Distance in mm from the tip apex to the orientation marker's centre (§2.2)                                                                       |
| `markerlen`        | float            | (directed leads only) Axial length of the orientation marker artifact (mm)                                                                                              |
| `eldist`           | float            | Centre-to-centre distance between adjacent contacts (mm). Computed automatically: `contact_spacing + contact_length` (uses second spacing value for non-uniform arrays) |
| `contact_span`     | float            | Axial length of the active array (mm), the manufacturer's array span. Computed automatically — see §2.1 and §2.2                                                        |


### 2.1 Computed Fields (auto-populated by `ea_resolve_elspec`)

```
# eldist (centre-to-centre pitch):
if length(contact_spacing) > 1:
    eldist = contact_spacing[1] + contact_length   # uses 2nd element (0-indexed)
else:
    eldist = contact_spacing + contact_length

# contact_span (axial span of active region):
n_levels = len(etageidx)
if length(contact_spacing) > 1:
    contact_span = (tip_length * tipiscontact
                    + contact_length * (n_levels - tipiscontact)
                    + sum(contact_spacing))
else:
    contact_span = (tip_length * tipiscontact
                    + contact_length * (n_levels - tipiscontact)
                    + contact_spacing * (n_levels - 1))
```

### 2.2 Geometry rules (normative, 2026-11-14)

These rules replace any conflicting statement elsewhere in this specification. They were checked against the reference catalogue, the reference contact meshes and the manufacturers' published dimensions (decisions of 2026-11-14).

- **Array span (`contact_span`).** The axial length of the active array, from the distal edge of the most distal contact (the tip apex when the tip is a contact) to the proximal edge of the most proximal contact, computed over levels with the §2.1 formula. It equals the manufacturer's array span (Boston Vercise Directed 7.5 mm, Cartesia HX 15.5 mm, Cartesia X 11.5 mm, Medtronic 3389 / B33005 and Abbott 6172 7.5 mm, NuMed Mini Lead 3.5 mm). No model overrides it. It is used only for the trajectory extension (`05` §5) and never as the head-to-tail distance.
- **Model fiducials.** In the model frame (+z proximal along the axis) the **head** fiducial is the centre of the most distal level. The **tail** fiducial is the centre of the **4th level** counted from the distal end, or of the last level when the model has fewer than 4 levels; SceneRay SR1202 and SR1212 use their 8th contact (their last level). The model `x` / `y` fiducials lie at radius `r = lead_diameter / 2` along model +x / +y at the head depth.
- **Fiducial span (`fiducial_span`).** The tail fiducial depth minus the head fiducial depth (mm): 6.0 mm for Boston Vercise Directed, Vercise (8 rings), Cartesia HX and Cartesia X and for depth leads of 2 mm pitch, 3.0 mm for NuMed Mini Lead, 0 for a single-level model. It is the expected `|tail − head|`: reconstruction plausibility, TRAC/CORE tail placement, the axial stretch `s` of `05` §6 and every model-to-marker fit use it.
- **Segment order.** On every directional level the first segment (A) lies at the model +Y direction, which is the orientation-marker direction and the zero of the roll angle. Viewed from the proximal end (model +x to the right, +y up), segments B and C follow at +120° and +240° (counter-clockwise: B at 210°, C at 330° measured from +x) for Boston Scientific and Medtronic leads, and at −120° and −240° (clockwise: B at 330°, C at 210°) for Abbott Directed 6172 / 6173 and Aleva directSTIM. Medtronic's distal marker piece faces segment B.
- **Segment arc (`segment_arc_deg`).** Each segment of a directional level covers `segment_arc_deg` of the circumference, centred on its azimuth; the rest of the level is insulation. Values from the reference meshes: 90° (Boston Vercise Directed, Medtronic B33005 / B33015, Abbott 6172 / 6173), 100° (Boston Cartesia HX / X), 85° (Aleva directSTIM). The viewer and the volume conductor (`07`) use the same catalogue value.
- **Marker position.** `markerpos` is measured from the tip apex to the centre of the orientation marker. Marker centres in the reference meshes: 11 mm Boston Vercise Directed, 14.4 mm Medtronic B33005 (two pieces, 12.0–14.3 mm and 14.5–16.8 mm), 10.75 mm Abbott 6172, 20 mm Cartesia HX, 16 mm Cartesia X.
- **Aleva directSTIM.** Lead and tip diameter 1.35 mm with a rounded cap (the reference's tip diameter of 0.5 mm describes only the flat apex of that cap); no orientation-marker geometry; `markerpos` 13 mm is unverified and `markerlen` is not defined (null); automatic orientation detection (`06`) is not supported.
- **Medtronic B33005 / B33015.** The manufacturer's manual gives a lead diameter of 1.36 mm and a distal tip of 1.0 mm; the catalogue keeps the reference values 1.3 mm and 0.9 mm for parity with existing reconstructions (known deviation).

### 2.3 Contact display names (normative, 2026-11-14; implementation pending)

Contact display names follow the reference implementation. A name is `K` + a number (+ the segment letter `A` / `B` / `C` for Medtronic and Abbott directional levels), numbered distal to proximal from the per-model start of the side in the table below, numbers contiguous within a side. The side suffix is ` (R)` / ` (L)` in labels and is dropped or compacted (`K0R`) in file names. Level names: the ring contact's name; for Boston Scientific and Aleva directional levels `K<first>-<last>` (their segments carry plain numbers); for Medtronic and Abbott directional levels the level number. Names appear in the 3D viewer labels, 2D slice export file names, reports and trajectory-planning level names. Stimulation settings, VTA files, the reconstruction file and group data stay positional (contacts 1..N per side) and never store names.

| Models | Right start | Left start | Style |
| ------ | ----------- | ---------- | ----- |
| Medtronic 3387 / 3389 / 3391, PINS L301 / L302 / L303, NeuroPace (both) | K0 | K8 | numeric |
| Medtronic B33005 / B33015 | K0 | K0 | level + letter (K0, K1A–C, K2A–C, K3) |
| Boston Vercise, Vercise Directed | K9 | K1 | numeric (segments numbered) |
| Boston Cartesia HX / X | K17 | K1 | numeric (segments numbered) |
| Abbott ActiveTip (both) | K1 | K1 | numeric |
| Abbott Directed 6172 / 6173 | K1 | K1 | level + letter (K1, K2A–C, K3A–C, K4) |
| SceneRay SR1200 / SR1210 / SR1211 / SR1242 | K4 | K0 | numeric |
| SceneRay SR1202 / SR1212 | K8 | K0 | numeric |
| Aleva directSTIM Directed | K1 | K13 | numeric (segments numbered) |
| NuMed Mini Lead, ELAINE rat, FHC WU rat | K0 | K0 | numeric |
| SmartFlow NGS-NC-06 | K0 (K0, K1) | K3 (K3, K4) | numeric (the reference's gap is kept) |
| SEEG / depth leads (SDE, PMT, EPC, DIXI, AdTech) | K0 | see below | numeric |

Left start of SEEG / depth leads: SDE-08 S8 (both generations) 8, S10 10, S12 12, S14 16, S16 16; PMT -04 4, -06 6, -08 8, -10 10, -12 12, -14 14, every -16 16; EPC 05C 8, 15C 16; DIXI 05 5, 08 8, 10 10, 12 12, 15 15, 18 18; AdTech BF08 8, BF09 9, RD06 6, RD08 8, every RD10 10, SD06 6, SD08 8, SD10 (both) 10, SD14 14.

---

## 3. The `electrode` Struct — Field Reference

The `ea_elspec_*.m` builder functions produce a richer struct `electrode` that adds 3-D mesh data and precomputed contact coordinates. This struct is serialised to `<matfname>.mat`.


| Field             | Type      | Description                                                                                                                                                                                                                |
| ----------------- | --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `electrode_model` | string    | Equals `elspec.elmodel`                                                                                                                                                                                                    |
| `numContacts`     | int       | Equals `elspec.numContacts`                                                                                                                                                                                                |
| `isdirected`      | 0 or 1    | Equals `elspec.isdirected`                                                                                                                                                                                                 |
| `contact_color`   | float     | Equals `elspec.contact_color`                                                                                                                                                                                              |
| `lead_color`      | float     | Equals `elspec.lead_color`                                                                                                                                                                                                 |
| `coords_mm`       | N×3 float | Contact centroid coordinates in the electrode's local frame (mm). N = `numContacts`. For directed contacts the centroid is offset radially to the surface of the lead shaft. Row `i` corresponds to contact `i` (1-based). |
| `head_position`   | 1×3 float | Centre of the most-distal (deepest) contact in local frame                                                                                                                                                                 |
| `tail_position`   | 1×3 float | Tail fiducial of §2.2 (centre of the 4th level, or of the last level when fewer)                                                                                                                                                                         |
| `x_position`      | 1×3 float | A point on the +X half of the shaft at the depth of `head_position`, used to define the local coordinate axes                                                                                                              |
| `y_position`      | 1×3 float | A point on the +Y half of the shaft at the depth of `head_position`                                                                                                                                                        |
| `contacts`        | struct[]  | Length = `numContacts`. Each element: `.vertices` (M×3 float) and `.faces` (F×3 int) — triangle mesh of one contact                                                                                                        |
| `insulation`      | struct[]  | Variable length. Each element: `.vertices` and `.faces` — triangle mesh of one insulation segment (shaft section, tip, or spacing between contacts)                                                                        |
| `meshel`          | struct    | (procedural models only) `.ins{}` and `.con{}` arrays describing cylinder primitives for volumetric mesh generation                                                                                                        |


### 3.1 Local Coordinate Frame Convention

All coordinates in `electrode.coords_mm` use a **local lead frame**:

- **Z axis** points proximally (toward the IPG) — i.e. from tip toward tail.
- The lead axis lies along Z.
- X and Y are radial; their absolute orientation is arbitrary in the local frame but defined by `x_position` and `y_position`.
- Z = 0 at the electrode tip apex.

### 3.2 Segmented Contact Centroid Convention

For directed (segmented) contacts, 3 segments occupy one axial level. Their centroids are placed at:

```
cx = (lead_diameter / 2) * cos(π/6)   # ≈ 0.866 × radius
cy = (lead_diameter / 2) * sin(π/6)   # = 0.5  × radius

Segment A (index 0):  offset = [  0,    +r,   0 ]   # Y+ direction
Segment B (index 1):  offset = [ -cx,  -cy,   0 ]   # 210° CCW from +Y
Segment C (index 2):  offset = [ +cx,  -cy,   0 ]   # 330° CCW from +Y
```

where `r = lead_diameter / 2`. The three segments are arranged 120° apart, counter-clockwise as seen from the proximal end for Boston Scientific and Medtronic leads; Abbott Directed 6172 / 6173 and Aleva directSTIM are clockwise (B and C swapped, §2.2).

### 3.3 Mesh File Format

Two `.mat` files exist per model:


| File                 | Contents                                                                                                          |
| -------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `<matfname>.mat`     | The `electrode` struct above — surface triangle meshes + contact coordinates                                      |
| `<matfname>_vol.mat` | Volumetric tetrahedral mesh: `node` (V×3) and `face` (F×4) for FEM use. Built by `readtetgen` from TetGen output. |


Source mesh files (TetGen input/output) live in model-specific subdirectories under `templates/electrode_models/`:

```
<ModelFolderName>/
    Contacts/
        con1.1.node  con1.1.ele  con1.1.face   (or .smesh)
        con2.1.*
        ...
    Insulations/
        ins1.1.node  ins1.1.ele  ins1.1.face   (or .smesh)
        ...
    final.1.*    (full volumetric mesh for _vol.mat)
```

The file extension used depends on generation tool:

- `.smesh` / `.1` (TetGen via SketchUp) — older models (Boston Vercise Directed, Abbott Directed, Medtronic B33005/15, Aleva, SmartFlow, ELAINE, FHC, NuMed)
- `.ele` / `.node` / `.face` (TetGen via Blender) — newer models (BSci Cartesia HX/X)

---

## 4. `ea_resolve_elspec` — Function Specification

### 4.1 Signature

```matlab
varargout = ea_resolve_elspec(varargin)
```

### 4.2 Behaviour by Call Mode

**Mode A — No arguments:** returns catalogue

```matlab
[display_names, matfnames] = ea_resolve_elspec()
% display_names : 73×1 cell array of human-readable model names
% matfnames     : 73×1 cell array of matfname identifiers
```

**Mode B — options struct argument:** returns populated elspec

```matlab
options_out = ea_resolve_elspec(options_in)
% options_in  : struct with field .elmodel (string, one of display_names)
%               If .elmodel is absent, attempts to load from reconstruction .mat;
%               falls back to 'Medtronic 3389' with a warning.
% options_out : options_in with new field .elspec populated
```

### 4.3 Lookup Mechanism

`ea_resolve_elspec` uses a large `switch/case` on the string `elmodel`. Each case directly assigns all `elspec` fields as scalar literals. There is no external database file or JSON; the geometry is hardcoded in this single function. After the switch block, `eldist` and `contact_span` are computed automatically.

### 4.4 Wyss DBS Localizer Target

```python
# dbsloc/electrodes/registry.py
def get_elspec(model_name: str) -> ElectrodeSpec: ...
def list_models() -> list[tuple[str, str]]: ...  # (display_name, matfname)
```

The registry should be backed by the `ElectrodeSpec` dataclass defined in §5.

---

## 5. `ElectrodeSpec` Dataclass (Wyss DBS Localizer Target)

```python
# dbsloc/electrodes/spec.py
from dataclasses import dataclass, field
from typing import Union
import numpy as np

@dataclass(frozen=True)
class ElectrodeSpec:
    # Identity
    elmodel: str                        # display name
    matfname: str                       # file base name / dict key

    # Shaft geometry
    lead_diameter: float                # mm
    tip_diameter: float                 # mm
    tip_length: float                   # mm
    tipiscontact: bool                  # tip is electrical contact

    # Contact geometry
    numContacts: int
    contact_length: float               # mm
    contact_diameter: float             # mm
    contact_spacing: Union[float, list[float]]  # mm edge-to-edge; list = non-uniform

    # Derived (computed at construction)
    eldist: float                       # mm centre-to-centre pitch
    contact_span: float                 # mm total active length

    # Electrical / stimulation
    isdirected: bool
    forstimulation: bool
    contactnames: tuple[str, ...]       # length = 2 * numContacts
    etagenames: tuple[tuple[str,...], tuple[str,...]]
    etageidx: tuple[tuple[int,...], ...]  # level → 1-based contact indices

    # Marker (directed leads only; None for undirected)
    markerpos: float | None = None      # mm from tip apex to marker centre
    markerlen: float | None = None      # mm axial length of marker
```

---

## 6. Complete Model Catalogue

### 6.1 Summary Table — All 73 Models

Columns: **n** = numContacts, **Ø** = lead/contact diameter (mm), **CL** = contact length (mm), **CS** = contact spacing edge-to-edge (mm), **TL** = tip length (mm), **TIC** = tipiscontact, **Dir** = isdirected, **DiO** = DiODe support.


| #   | Display Name                          | matfname                   | Manufacturer      | n   | Ø (mm) | CL (mm) | CS (mm)           | TL (mm) | TIC   | Dir   | DiO   | Mesh dir                              |
| --- | ------------------------------------- | -------------------------- | ----------------- | --- | ------ | ------- | ----------------- | ------- | ----- | ----- | ----- | ------------------------------------- |
| 1   | Medtronic 3389                        | medtronic_3389             | Medtronic         | 4   | 1.27   | 1.5     | 0.5               | 1.5     | N     | N     | N     | —                                     |
| 2   | Medtronic 3387                        | medtronic_3387             | Medtronic         | 4   | 1.27   | 1.5     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 3   | Medtronic 3391                        | medtronic_3391             | Medtronic         | 4   | 1.27   | 3.0     | 4.0               | 1.5     | N     | N     | N     | —                                     |
| 4   | Medtronic B33005                      | medtronic_b33005           | Medtronic         | 8   | 1.3    | 1.5     | 0.5               | 0.9     | N     | **Y** | **Y** | Medtronic_B33005                      |
| 5   | Medtronic B33015                      | medtronic_b33015           | Medtronic         | 8   | 1.3    | 1.5     | 1.5               | 0.9     | N     | **Y** | **Y** | Medtronic_B33015                      |
| 6   | Boston Scientific Vercise             | boston_vercise             | Boston Scientific | 8   | 1.3    | 1.5     | 0.5               | 1.1     | N     | N     | N     | —                                     |
| 7   | Boston Scientific Vercise Directed    | boston_vercise_directed    | Boston Scientific | 8   | 1.3    | 1.5     | 0.5               | 1.5     | **Y** | **Y** | **Y** | Boston_Vercise_Directed               |
| 8   | Boston Scientific Vercise Cartesia HX | boston_vercise_cartesia_hx | Boston Scientific | 16  | 1.3    | 1.5     | 0.5               | 1.1     | N     | **Y** | **Y** | Boston_Scientific_Vercise_Cartesia_HX |
| 9   | Boston Scientific Vercise Cartesia X  | boston_vercise_cartesia_x  | Boston Scientific | 16  | 1.3    | 1.5     | 0.5               | 1.1     | N     | **Y** | **Y** | Boston_Scientific_Vercise_Cartesia_X  |
| 10  | Abbott ActiveTip (6146-6149)          | abbott_activetip_2mm       | Abbott            | 4   | 1.4    | 1.5     | 0.5               | 3.0     | **Y** | N     | N     | —                                     |
| 11  | Abbott ActiveTip (6142-6145)          | abbott_activetip_3mm       | Abbott            | 4   | 1.4    | 1.5     | 1.5               | 3.0     | **Y** | N     | N     | —                                     |
| 12  | Abbott Directed 6172 (short)          | abbott_directed_05         | Abbott            | 8   | 1.27   | 1.5     | 0.5               | 1.0     | N     | **Y** | **Y** | Abbott_Directed_05                    |
| 13  | Abbott Directed 6173 (long)           | abbott_directed_15         | Abbott            | 8   | 1.27   | 1.5     | 1.5               | 1.0     | N     | **Y** | **Y** | Abbott_Directed_15                    |
| 14  | SceneRay SR1200                       | sceneray_sr1200            | SceneRay          | 4   | 1.27   | 1.5     | 0.5               | 1.5     | N     | N     | N     | —                                     |
| 15  | SceneRay SR1202                       | sceneray_sr1202            | SceneRay          | 8   | 1.27   | 1.5     | 0.5               | 1.5     | N     | N     | N     | —                                     |
| 16  | SceneRay SR1210                       | sceneray_sr1210            | SceneRay          | 4   | 1.27   | 1.5     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 17  | SceneRay SR1211                       | sceneray_sr1211            | SceneRay          | 4   | 1.27   | 1.5     | 1.0               | 1.5     | N     | N     | N     | —                                     |
| 18  | SceneRay SR1212                       | sceneray_sr1212            | SceneRay          | 8   | 1.27   | 1.5     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 19  | SceneRay SR1242                       | sceneray_sr1242            | SceneRay          | 4   | 1.27   | 3.0     | 2.0/4.0/4.0†      | 1.5     | N     | N     | N     | —                                     |
| 20  | PINS Medical L301                     | pins_l301                  | PINS Medical      | 4   | 1.3    | 1.5     | 0.5               | 1.5     | N     | N     | N     | —                                     |
| 21  | PINS Medical L302                     | pins_l302                  | PINS Medical      | 4   | 1.3    | 1.5     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 22  | PINS Medical L303                     | pins_l303                  | PINS Medical      | 4   | 1.3    | 3.0     | 4.0               | 1.5     | N     | N     | N     | —                                     |
| 23  | SDE-08 S8 Legacy                      | sde_08_s8_legacy           | SDE               | 8   | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 24  | SDE-08 S10 Legacy                     | sde_08_s10_legacy          | SDE               | 10  | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 25  | SDE-08 S12 Legacy                     | sde_08_s12_legacy          | SDE               | 12  | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 26  | SDE-08 S16 Legacy                     | sde_08_s16_legacy          | SDE               | 16  | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     | —                                     |
| 27  | SDE-08 S8                             | sde_08_s8                  | SDE               | 8   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 28  | SDE-08 S10                            | sde_08_s10                 | SDE               | 10  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 29  | SDE-08 S12                            | sde_08_s12                 | SDE               | 12  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 30  | SDE-08 S14                            | sde_08_s14                 | SDE               | 14  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 31  | SDE-08 S16                            | sde_08_s16                 | SDE               | 16  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 32  | PMT 2102-04-091                       | pmt_2102_04_091            | PMT               | 4   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-04-091*                      |
| 33  | PMT 2102-06-091                       | pmt_2102_06_091            | PMT               | 6   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-06-091*                      |
| 34  | PMT 2102-08-091                       | pmt_2102_08_091            | PMT               | 8   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-08-091                       |
| 35  | PMT 2102-10-091                       | pmt_2102_10_091            | PMT               | 10  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-10-091*                      |
| 36  | PMT 2102-12-091                       | pmt_2102_12_091            | PMT               | 12  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-12-091*                      |
| 37  | PMT 2102-14-091                       | pmt_2102_14_091            | PMT               | 14  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-14-091*                      |
| 38  | PMT 2102-16-091                       | pmt_2102_16_091            | PMT               | 16  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-16-091                       |
| 39  | PMT 2102-16-092                       | pmt_2102_16_092            | PMT               | 16  | 0.8    | 2.0     | 1.97              | 2.0     | **Y** | N     | N     | PMT_2102-16-092                       |
| 40  | PMT 2102-16-093                       | pmt_2102_16_093            | PMT               | 16  | 0.8    | 2.0     | 2.43              | 2.0     | **Y** | N     | N     | PMT_2102-16-093                       |
| 41  | PMT 2102-16-131                       | pmt_2102_16_131            | PMT               | 16  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | PMT_2102-16-131                       |
| 42  | PMT 2102-16-142                       | pmt_2102_16_142            | PMT               | 16  | 0.8    | 2.0     | 3.53              | 2.0     | **Y** | N     | N     | PMT_2102-16-142                       |
| 43  | 2069-EPC-05C-35                       | epc_05c                    | EPC               | 5   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 44  | 2069-EPC-15C-35                       | epc_15c                    | EPC               | 15  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 45  | NeuroPace DL-344-3.5                  | neuropace_dl_344_35        | NeuroPace         | 4   | 1.27   | 2.0     | 1.5               | 1.1     | N     | N     | N     | —                                     |
| 46  | NeuroPace DL-344-10                   | neuropace_dl_344_10        | NeuroPace         | 4   | 1.27   | 2.0     | 8.0               | 1.1     | N     | N     | N     | —                                     |
| 47  | DIXI D08-05AM                         | dixi_d08_05am              | DIXI Medical      | 5   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 48  | DIXI D08-08AM                         | dixi_d08_08am              | DIXI Medical      | 8   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 49  | DIXI D08-10AM                         | dixi_d08_10am              | DIXI Medical      | 10  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 50  | DIXI D08-12AM                         | dixi_d08_12am              | DIXI Medical      | 12  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 51  | DIXI D08-15AM                         | dixi_d08_15am              | DIXI Medical      | 15  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 52  | DIXI D08-18AM                         | dixi_d08_18am              | DIXI Medical      | 18  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     | —                                     |
| 53  | AdTech BF08R-SP05X                    | adtech_bf08r_sp05x         | AdTech            | 8   | 1.28   | 1.57    | 3.43              | 1.0     | N     | N     | N     | —                                     |
| 54  | AdTech BF08R-SP21X                    | adtech_bf08r_sp21x         | AdTech            | 8   | 1.28   | 1.57    | 1.43/3.93×6‡      | 1.0     | N     | N     | N     | —                                     |
| 55  | AdTech BF08R-SP61X                    | adtech_bf08r_sp61x         | AdTech            | 8   | 1.28   | 1.57    | 1.43/4.43×6‡      | 1.0     | N     | N     | N     | —                                     |
| 56  | AdTech BF09R-SP61X-0BB                | adtech_bf09r_sp61x_0bb     | AdTech            | 9   | 1.28   | 1.57    | 1.43/4.43×7‡      | 1.0     | N     | N     | N     | —                                     |
| 57  | AdTech RD06R-SP05X                    | adtech_rd06r_sp05x         | AdTech            | 6   | 0.86   | 2.29    | 2.71              | 2.0     | N     | N     | N     | —                                     |
| 58  | AdTech RD08R-SP05X                    | adtech_rd08r_sp05x         | AdTech            | 8   | 0.86   | 2.29    | 2.71              | 2.0     | N     | N     | N     | —                                     |
| 59  | AdTech RD10R-SP03X                    | adtech_rd10r_sp03x         | AdTech            | 10  | 0.86   | 2.29    | 0.71              | 2.0     | N     | N     | N     | —                                     |
| 60  | AdTech RD10R-SP05X                    | adtech_rd10r_sp05x         | AdTech            | 10  | 0.86   | 2.29    | 2.71              | 2.0     | N     | N     | N     | —                                     |
| 61  | AdTech RD10R-SP06X                    | adtech_rd10r_sp06x         | AdTech            | 10  | 0.86   | 2.29    | 3.71              | 2.0     | N     | N     | N     | —                                     |
| 62  | AdTech RD10R-SP07X                    | adtech_rd10r_sp07x         | AdTech            | 10  | 0.86   | 2.29    | 4.71              | 2.0     | N     | N     | N     | —                                     |
| 63  | AdTech RD10R-SP08X                    | adtech_rd10r_sp08x         | AdTech            | 10  | 0.86   | 2.29    | 5.71              | 2.0     | N     | N     | N     | —                                     |
| 64  | AdTech SD06R-SP26X                    | adtech_sd06r_sp26x         | AdTech            | 6   | 1.12   | 2.41    | 2.59/2.59/7.59×3§ | 2.0     | N     | N     | N     | —                                     |
| 65  | AdTech SD08R-SP05X                    | adtech_sd08r_sp05x         | AdTech            | 8   | 1.12   | 2.41    | 2.59              | 2.0     | N     | N     | N     | —                                     |
| 66  | AdTech SD10R-SP05X                    | adtech_sd10r_sp05x         | AdTech            | 10  | 1.12   | 2.41    | 2.59              | 2.0     | N     | N     | N     | —                                     |
| 67  | AdTech SD10R-SP05X Choi               | adtech_sd10r_sp05x_choi    | AdTech            | 10  | 1.1    | 2.4     | 2.4               | 1.0     | N     | N     | N     | —                                     |
| 68  | AdTech SD14R-SP05X                    | adtech_sd14r_sp05x         | AdTech            | 14  | 1.12   | 2.41    | 2.59              | 2.0     | N     | N     | N     | —                                     |
| 69  | ELAINE Rat Electrode                  | elaine_rat_electrode       | ELAINE (research) | 1   | 0.225  | 0.0     | 0.0               | 0.1125  | **Y** | N     | N     | ELAINE_Rat_Electrode                  |
| 70  | FHC WU Rat Electrode                  | fhc_wu_rat_electrode       | FHC / WU          | 1   | 0.125  | 0.0     | 0.0               | 0.250   | **Y** | N     | N     | FHC_WU_Rat_Electrode                  |
| 71  | NuMed Mini Lead                       | numed_minilead             | NuMed             | 4   | 0.3125 | 0.5     | 0.5               | 0.5     | N     | N     | N     | NuMedMiniLead                         |
| 72  | Aleva directSTIM Directed             | aleva_directstim_directed  | Aleva             | 12  | 1.35   | 1.5     | 0.5               | 1.1     | N     | **Y** | N     | Aleva_directSTIM_Directed             |
| 73  | SmartFlow Cannula NGS-NC-06           | smartflow_ngs-nc-06        | SmartFlow         | 2   | 1.65†  | 10.0    | 0.0               | 3.0     | **Y** | N     | N     | SmartFlow_NGS-NC-06                   |


**Footnotes:**

- † SR1242: non-uniform `contact_spacing = [2.0, 4.0, 4.0]` mm (3 gaps between 4 contacts)
- ‡ BF08R-SP21X: `[1.43, 3.93, 3.93, 3.93, 3.93, 3.93, 3.93]`; BF08R-SP61X: `[1.43, 4.43×6]`; BF09R-SP61X-0BB: `[1.43, 4.43×7]`
- § SD06R-SP26X: `[2.59, 2.59, 7.59, 7.59, 7.59]`
- † SmartFlow: `lead_diameter=1.65` (cannula OD), `contact_diameter=0.27` (tip hole), `tip_diameter=0.2`; dimensions are atypical
- PMT folders: only PMT_2102-08-091, -16-091, -16-092, -16-093, -16-131, -16-142 have mesh subdirectories; others use procedural meshes

---

## 7. Detailed Per-Model Entries

### 7.1 Medtronic Classic Series (3387, 3389, 3391)

All three are **4-contact, ring-electrode DBS leads** sharing the same shaft diameter.

#### Medtronic 3389

- **Clinical use:** STN-DBS, GPi-DBS (compact spacing, used where target is small)
- **Contacts:** 4 rings, K0–K3 right / K8–K11 left
- **Geometry:**
  - Shaft Ø 1.27 mm, contact Ø 1.27 mm
  - Contact length 1.5 mm, edge-to-edge gap 0.5 mm → c-t-c pitch 2.0 mm
  - Tip length 1.5 mm (non-contact insulating cap)
  - Total active span: 0 + 4×1.5 + 3×0.5 = 7.5 mm
- **Mesh:** procedural (generated by `ea_elspec_medtronic_3389.m` using cylinder primitives)
- **DiODe:** not applicable (no directionality)

#### Medtronic 3387

- **Clinical use:** STN-DBS (wider spacing variant)
- **Identical to 3389 except:** `contact_spacing = 1.5 mm` → c-t-c pitch 3.0 mm
- **Total active span:** 4×1.5 + 3×1.5 = 10.5 mm

#### Medtronic 3391

- **Clinical use:** Spinal cord stimulation, GPi-DBS (macro contacts, long spacing)
- **Contacts:** 4 rings
- **Geometry:**
  - Contact length 3.0 mm, edge-to-edge gap 4.0 mm → c-t-c pitch 7.0 mm
  - Tip length 1.5 mm
  - Total active span: 4×3.0 + 3×4.0 = 24.0 mm

---

### 7.2 Medtronic SureTune/Percept Series (B33005, B33015)

Both are **8-contact directional leads** with layout: ring – 3-seg – 3-seg – ring.

#### Medtronic B33005 (0.5 mm spacing)

- **Clinical use:** STN-DBS, GPi-DBS (directional stimulation, compact spacing)
- **Contacts:** K0 (ring), K1A/B/C (seg), K2A/B/C (seg), K3 (ring) — 8 total
- **Geometry:**
  - Shaft Ø 1.3 mm
  - Contact length 1.5 mm, spacing 0.5 mm, tip length 0.9 mm
  - c-t-c pitch 2.0 mm
  - Active span: 4×1.5 + 3×0.5 = 7.5 mm
  - **Marker:** `markerpos = 14.4 mm`, `markerlen = 2.3 mm`
- **Contact z-coordinates (local frame, from `ea_elspec_medtronic_b33005.m`):**
  - K0: z = 1.65 mm (ring, on-axis)
  - K1A/B/C: z = 3.65 mm (offset radially ±r)
  - K2A/B/C: z = 5.65 mm (offset radially ±r)
  - K3: z = 7.65 mm (ring, on-axis)
- **Segmented contact rotation:** CCW viewed from proximal (A=+Y, B=lower-left, C=lower-right)
- **Mesh source:** `Medtronic_B33005/Contacts/con*.smesh` + `Insulations/ins*.smesh`
- **DiODe:** `ea_diode_medtronic` (Medtronic-specific algorithm using marker artifact)

#### Medtronic B33015 (1.5 mm spacing)

- **Identical to B33005 except:** `contact_spacing = 1.5 mm` → c-t-c pitch 3.0 mm
- **Contact z-coordinates:**
  - K0: z = 1.65 mm
  - K1A/B/C: z = 4.65 mm
  - K2A/B/C: z = 7.65 mm
  - K3: z = 10.65 mm
- **Active span:** 4×1.5 + 3×1.5 = 10.5 mm
- **Marker:** `markerpos = 17.4 mm`, `markerlen = 2.3 mm`

---

### 7.3 Boston Scientific Vercise Series

#### Boston Scientific Vercise (omnidirectional)

- **Clinical use:** DBS (8-contact ring lead)
- **Contacts:** 8 rings, K9–K16 right / K1–K8 left
- **Geometry:**
  - Shaft Ø 1.3 mm, contact length 1.5 mm, spacing 0.5 mm, tip 1.1 mm (non-contact)
  - c-t-c pitch 2.0 mm, active span: 8×1.5 + 7×0.5 = 15.5 mm

#### Boston Scientific Vercise Directed

- **Clinical use:** STN/GPi DBS (directional)
- **Contact layout:** ring – 3-seg – 3-seg – ring (8 contacts, **tipiscontact=1**)
- **Geometry:**
  - Shaft Ø 1.3 mm, contact length 1.5 mm, spacing 0.5 mm, tip 1.5 mm (IS a contact)
  - Marker: `markerpos = 11 mm`, `markerlen = 3 mm`
- **Contact z-coordinates (from `ea_elspec_boston_vercise_directed.m`):**
  - K9: z = 0.75 mm (ring, tip contact, on-axis)
  - K10–12: z = 2.75 mm (3 segments)
  - K13–15: z = 4.75 mm (3 segments)
  - K16: z = 6.75 mm (ring)
- **DiODe:** `ea_diode_auto` (FFT-based, CT artifact)

#### Boston Scientific Vercise Cartesia HX (16-contact)

- **Clinical use:** DBS (8-level: 4 directed + 4 ring levels)
- **Contact layout:** 3-seg × 4 levels + ring × 4 levels (16 contacts total)
- **etageidx:** `{1:3, 4:6, 7:9, 10:12, 13, 14, 15, 16}` → 8 groups
- **Geometry:**
  - Shaft Ø 1.3 mm, contact length 1.5 mm, spacing 0.5 mm, tip 1.1 mm (non-contact)
  - c-t-c pitch 2.0 mm
  - Marker: `markerpos = 20 mm`, `markerlen = 3 mm`
- **Contact z-coordinates (formula: tip + (level−1)×pitch + CL/2):**
  - Levels 1–4 (directed, 3 segs each): z = 1.85, 3.85, 5.85, 7.85 mm
  - Levels 5–8 (ring): z = 9.85, 11.85, 13.85, 15.85 mm
- **Mesh source:** `Boston_Scientific_Vercise_Cartesia_HX/` (Blender+TetGen, `.ele` format)
- **DiODe:** `ea_diode_auto`

#### Boston Scientific Vercise Cartesia X (16-contact)

- **Clinical use:** DBS (6-level: 5 directed + 1 ring)
- **Contact layout:** 3-seg × 5 levels + ring × 1 (16 contacts)
- **etageidx:** `{1:3, 4:6, 7:9, 10:12, 13:15, 16}` → 6 groups
- **Geometry:** identical shaft/contact dims to Cartesia HX
- **Marker:** `markerpos = 16 mm`, `markerlen = 3 mm`
- **Active span:** 6 levels × 1.5 + 5 × 0.5 = 11.5 mm
- **Mesh source:** `Boston_Scientific_Vercise_Cartesia_X/` (Blender+TetGen, `.ele` format)
- **DiODe:** `ea_diode_auto`

---

### 7.4 Abbott (St. Jude Medical) Series

#### Abbott ActiveTip (6146-6149) — "2 mm spacing"

- **Clinical use:** DBS
- **Contacts:** 4 rings, **tipiscontact=1** (tip IS K1)
- **Geometry:**
  - Shaft Ø 1.4 mm, contact length 1.5 mm, spacing 0.5 mm, tip 3.0 mm
  - The 3.0 mm tip is electrically active as K1 (most distal contact)
  - Active span: 3.0 + 3×1.5 + 3×0.5 = 9.0 mm

#### Abbott ActiveTip (6142-6145) — "3 mm spacing"

- **Identical to 6146-6149 except:** `contact_spacing = 1.5 mm`
- **Active span:** 3.0 + 3×1.5 + 3×1.5 = 12.0 mm

#### Abbott Directed 6172 (short) — 0.5 mm spacing

- **Clinical use:** STN/GPi DBS (directional)
- **Contact layout:** ring – 3-seg – 3-seg – ring (8 contacts, **tipiscontact=0**)
- **Geometry:**
  - Shaft Ø 1.27 mm, contact length 1.5 mm, spacing 0.5 mm, tip 1.0 mm (non-contact)
  - Marker: `markerpos = 10.75 mm`, `markerlen = 1.5 mm`
- **Contact z-coordinates (from `ea_elspec_abbott_directed_05.m`):**
  - K1: z = 1.75 mm (ring, on-axis)
  - K2A/B/C: z = 3.75 mm (3 segments; A=+Y, B=lower-right, C=lower-left — note reversed vs Medtronic)
  - K3A/B/C: z = 5.75 mm
  - K4: z = 7.75 mm (ring)
- **Active span:** 4×1.5 + 3×0.5 = 7.5 mm
- **DiODe:** `ea_diode_auto`

> **Note on segment ordering:** The Abbott directed files use `[cx, -cy]` for seg B and `[-cx, -cy]` for seg C, which is the reverse of Medtronic B33005/B33015. Aleva also has its own ordering. Implementations must respect the per-model ordering encoded in `coords_mm`.

#### Abbott Directed 6173 (long) — 1.5 mm spacing

- **Identical to 6172 except:** `contact_spacing = 1.5 mm`
- **Contact z-coordinates:**
  - K1: z = 1.75 mm
  - K2A/B/C: z = 4.75 mm
  - K3A/B/C: z = 7.75 mm
  - K4: z = 10.75 mm
- **Active span:** 4×1.5 + 3×1.5 = 10.5 mm
- **Marker:** `markerpos = 13.75 mm`, `markerlen = 1.5 mm`
- **DiODe:** `ea_diode_auto`

---

### 7.5 SceneRay Series

Chinese DBS leads; geometry closely mirrors Medtronic 3387/3389.


| Model  | Contacts | CL (mm) | CS (mm)     | Notes                               |
| ------ | -------- | ------- | ----------- | ----------------------------------- |
| SR1200 | 4        | 1.5     | 0.5         | K4-K7 right, K0-K3 left             |
| SR1202 | 8        | 1.5     | 0.5         | K8-K15 right, K0-K7 left            |
| SR1210 | 4        | 1.5     | 1.5         | like 3387                           |
| SR1211 | 4        | 1.5     | 1.0         | intermediate spacing                |
| SR1212 | 8        | 1.5     | 1.5         | 8-contact wide spacing              |
| SR1242 | 4        | 3.0     | 2.0/4.0/4.0 | macro contacts, non-uniform spacing |


SR1242 has notably large contacts (3.0 mm) and non-uniform gap array `[2.0, 4.0, 4.0]` mm.

---

### 7.6 PINS Medical Series (L301, L302, L303)

Chinese DBS leads; geometry mirrors Medtronic 3389/3387/3391 almost exactly.


| Model | Contacts | CL (mm) | CS (mm) | Notes                         |
| ----- | -------- | ------- | ------- | ----------------------------- |
| L301  | 4        | 1.5     | 0.5     | K0-K3 right, K8-K11 left      |
| L302  | 4        | 1.5     | 1.5     |                               |
| L303  | 4        | 3.0     | 4.0     | macro; mirrors Medtronic 3391 |


---

### 7.7 SDE-08 Series (SEEG)

Thin-shaft (Ø 0.8 mm) SEEG depth electrodes available in 8–16 contact counts and two generations (Legacy vs current). All have uniform 1.5 mm edge-to-edge spacing and 2.0 mm contact length (c-t-c pitch 3.5 mm).

**Legacy models** (`sde_08_s{8,10,12,16}_legacy`): `tipiscontact=0`, `tip_length=1.5 mm` — tip is an insulating cap.

**Current models** (`sde_08_s{8,10,12,14,16}`): `tipiscontact=1`, `tip_length=2.0 mm` — tip is the first contact.

The current series adds a **14-contact** variant (SDE-08 S14) not present in the legacy series.

---

### 7.8 PMT Series (SEEG)

PMT Medical SEEG electrodes. All share Ø 0.8 mm shaft, 2.0 mm contact length, `tipiscontact=1`, `tip_length=2.0 mm`. The variant suffix encodes contact count (04–16) and contact spacing:


| Suffix  | Contacts | CS (mm) | Centre-to-centre pitch |
| ------- | -------- | ------- | ---------------------- |
| -04-091 | 4        | 1.5     | 3.5                    |
| -06-091 | 6        | 1.5     | 3.5                    |
| -08-091 | 8        | 1.5     | 3.5                    |
| -10-091 | 10       | 1.5     | 3.5                    |
| -12-091 | 12       | 1.5     | 3.5                    |
| -14-091 | 14       | 1.5     | 3.5                    |
| -16-091 | 16       | 1.5     | 3.5                    |
| -16-092 | 16       | 1.97    | 3.97                   |
| -16-093 | 16       | 2.43    | 4.43                   |
| -16-131 | 16       | 1.5     | 3.5                    |
| -16-142 | 16       | 3.53    | 5.53                   |


PMT_2102-16-131 and -16-091 share the same elspec parameters (both have 1.5 mm spacing) but differ in their physical lead length/construction and have separate mesh subdirectories.

---

### 7.9 EPC Series (PMT)

Two models from the EPC (Epilepsy Probe Catheter) line:


| Model           | Contacts | CS (mm) |
| --------------- | -------- | ------- |
| 2069-EPC-05C-35 | 5        | 1.5     |
| 2069-EPC-15C-35 | 15       | 1.5     |


All parameters otherwise identical to PMT SEEG series (Ø 0.8 mm, CL 2.0 mm, tipiscontact=1).

---

### 7.10 NeuroPace Series (RNS)

Used with the NeuroPace RNS System for responsive neurostimulation.


| Model      | matfname            | CS (mm) | Notes                |
| ---------- | ------------------- | ------- | -------------------- |
| DL-344-3.5 | neuropace_dl_344_35 | 1.5     | Standard spacing     |
| DL-344-10  | neuropace_dl_344_10 | 8.0     | Wide-spacing variant |


Both: Ø 1.27 mm shaft, CL 2.0 mm, `tipiscontact=0`, `tip_length=1.1 mm`. 4 ring contacts.

---

### 7.11 DIXI Medical Series (SEEG)

DIXI MICRODEEP depth electrodes. All: Ø 0.8 mm, CL 2.0 mm, CS 1.5 mm, `tipiscontact=1`, `tip_length=2.0 mm`.


| Model    | Contacts |
| -------- | -------- |
| D08-05AM | 5        |
| D08-08AM | 8        |
| D08-10AM | 10       |
| D08-12AM | 12       |
| D08-15AM | 15       |
| D08-18AM | 18       |


---

### 7.12 AdTech Series (SEEG / ECoG / Depth)

AdTech manufactures several electrode families with different shaft diameters and spacing conventions. Unlike SEEG leads above, AdTech electrodes use `tipiscontact=0` (insulating tip).

#### AdTech BF (Behnke-Fried) — Hippocampal depth + microwire

Shaft Ø 1.28 mm, CL 1.57 mm, tip 1.0 mm, `tipiscontact=0`


| Model           | n   | CS (mm)        | Notes                             |
| --------------- | --- | -------------- | --------------------------------- |
| BF08R-SP05X     | 8   | 3.43 (uniform) | Uniform spacing                   |
| BF08R-SP21X     | 8   | [1.43, 3.93×6] | Close proximal pair               |
| BF08R-SP61X     | 8   | [1.43, 4.43×6] | Close proximal pair, wider distal |
| BF09R-SP61X-0BB | 9   | [1.43, 4.43×7] | 9-contact variant, no ball at end |


Note: `eldist` for non-uniform arrays uses `contact_spacing[1] + contact_length` (i.e. the second gap value).

#### AdTech RD (Round) — Depth electrodes

Shaft Ø 0.86 mm, CL 2.29 mm, tip 2.0 mm, `tipiscontact=0`. The model suffix `SPxxX` encodes the inter-contact spacing in tenths of mm (approximately):


| Model       | n   | CS (mm) |
| ----------- | --- | ------- |
| RD06R-SP05X | 6   | 2.71    |
| RD08R-SP05X | 8   | 2.71    |
| RD10R-SP03X | 10  | 0.71    |
| RD10R-SP05X | 10  | 2.71    |
| RD10R-SP06X | 10  | 3.71    |
| RD10R-SP07X | 10  | 4.71    |
| RD10R-SP08X | 10  | 5.71    |


The naming convention: centre-to-centre pitch codes. SP05X = 5 mm c-t-c → CS = 5.0 − 2.29 = 2.71 mm; SP03X = 3 mm c-t-c → CS = 0.71 mm; etc.

#### AdTech SD (Spencer-Depth) — Temporal depth electrodes

Shaft Ø 1.12 mm, CL 2.41 mm, tip 2.0 mm, `tipiscontact=0`.


| Model            | n   | CS (mm)                        | Notes                                                     |
| ---------------- | --- | ------------------------------ | --------------------------------------------------------- |
| SD06R-SP26X      | 6   | [2.59, 2.59, 7.59, 7.59, 7.59] | Mixed spacing: 2 close + 3 wide                           |
| SD08R-SP05X      | 8   | 2.59                           |                                                           |
| SD10R-SP05X      | 10  | 2.59                           |                                                           |
| SD10R-SP05X Choi | 10  | 2.4                            | Slightly different (Choi variant); Ø 1.1, CL 2.4, tip 1.0 |
| SD14R-SP05X      | 14  | 2.59                           |                                                           |


SD06R-SP26X has a mixed-spacing array `[2.59, 2.59, 7.59, 7.59, 7.59]` representing two densely-spaced proximal contacts and widely-spaced distal ones.

---

### 7.13 Aleva directSTIM Directed

- **Manufacturer:** Aleva Neurotherapeutics
- **Clinical use:** DBS (all-directional: 4 levels × 3 segments each)
- **Contact layout:** 3-seg × 4 levels = 12 contacts; **no ring contacts at any level**
- **etageidx:** `{1:3, 4:6, 7:9, 10:12}` → 4 groups, each fully segmented
- **Geometry:**
  - Shaft and tip Ø 1.35 mm with a rounded cap (§2.2), `tip_length=1.1 mm`, `tipiscontact=0`
  - Contact length 1.5 mm, spacing 0.5 mm → pitch 2.0 mm
- **Contact z-coordinates (from `ea_elspec_aleva_directstim_directed.m`):**
  - Level 1 (K1/2/3): z = 1.82 mm
  - Level 2 (K4/5/6): z = 3.82 mm
  - Level 3 (K7/8/9): z = 5.82 mm
  - Level 4 (K10/11/12): z = 7.82 mm
- **Segmented contact rotation:** **CLOCKWISE** from +Y viewed from proximal (A=+Y, B=lower-right, C=lower-left) — opposite to Boston/Medtronic
- **Marker:** none; `markerpos = 13 mm` unverified, `markerlen` null, orientation detection unsupported (§2.2)
- **Mesh source:** `Aleva_directSTIM_Directed/Contacts/*.face` + `Insulations/*.face`
- **DiODe:** NOT supported (no implementation in `ea_diode_main.m`)

---

### 7.14 SmartFlow Cannula NGS-NC-06

- **Manufacturer:** SmartFlow Neuroscience
- **Type:** Convection-enhanced delivery (CED) cannula — not a traditional DBS lead
- **Contacts:** 2; `tipiscontact=1`
- **Geometry (atypical):**
  - Cannula outer Ø 1.65 mm, contact Ø 0.27 mm (tip-hole), tip Ø 0.2 mm
  - `contact_length = 10.0 mm` (very long, represents an infusion window)
  - `tip_length = 3.0 mm`, `contact_spacing = 0.0`
  - Z-coordinates: K0 at z = 1.5 mm (tip midpoint), K1 at z = 8.0 mm
- **Mesh source:** `SmartFlow_NGS-NC-06/`
- **DiODe:** not applicable

---

### 7.15 Research / Animal Electrodes

#### ELAINE Rat Electrode

- **Single contact** (`numContacts=1`, `tipiscontact=1`)
- Shaft Ø **0.225 mm** (225 µm), tip length **0.1125 mm** (112.5 µm)
- `contact_length=0`, `contact_spacing=0` — entire tip IS the contact
- Contact z-coordinate: z = `tip_length = 0.1125 mm`
- Mesh: `ELAINE_Rat_Electrode/`

#### FHC WU Rat Electrode

- **Single contact** (`numContacts=1`, `tipiscontact=1`)
- Shaft Ø **0.125 mm** (125 µm), tip length **0.250 mm** (250 µm)
- `contact_length=0`, `contact_spacing=0`
- Contact z-coordinate: z = 0.250 mm
- Mesh: `FHC_WU_Rat_Electrode/`

#### NuMed Mini Lead

- **4-contact** paediatric / small-target lead
- Shaft Ø **0.3125 mm**, contact length **0.5 mm**, spacing **0.5 mm**, tip **0.5 mm**
- `tipiscontact=0`; c-t-c pitch 1.0 mm
- Mesh: `NuMedMiniLead/`

---

## 8. DiODe Support Matrix

DiODe (Direction of DBS electrode) detects the rotational orientation of directional leads from postoperative CT images.


| Lead                                  | Algorithm            | Notes                                                 |
| ------------------------------------- | -------------------- | ----------------------------------------------------- |
| Boston Scientific Vercise Directed    | `ea_diode_auto`      | FFT-based CT artifact analysis                        |
| Boston Scientific Vercise Cartesia HX | `ea_diode_auto`      | same                                                  |
| Boston Scientific Vercise Cartesia X  | `ea_diode_auto`      | same                                                  |
| Abbott Directed 6172 (short)          | `ea_diode_auto`      | same                                                  |
| Abbott Directed 6173 (long)           | `ea_diode_auto`      | same                                                  |
| Medtronic B33005                      | `ea_diode_medtronic` | Medtronic-specific marker-based algorithm             |
| Medtronic B33015                      | `ea_diode_medtronic` | same                                                  |
| Aleva directSTIM Directed             | **not supported**    | `isdirected=1` but no DiODe path in `ea_diode_main.m` |


All other models: DiODe not applicable (no directional contacts).

---

## 9. `elspec` Hierarchy / Categorisation

The codebase uses no explicit taxonomy, but models can be grouped functionally:

### 9.1 By Use Category


| Category              | Models                                                                                                    |
| --------------------- | --------------------------------------------------------------------------------------------------------- |
| **DBS — ring**        | Medtronic 3387/3389/3391, BSci Vercise, Abbott ActiveTip 6142/6146, SceneRay SR12xx, PINS L301/L302/L303  |
| **DBS — directional** | Medtronic B33005/B33015, BSci Vercise Directed/Cartesia HX/X, Abbott Directed 6172/6173, Aleva directSTIM |
| **SEEG**              | SDE-08 series, PMT series, EPC series, DIXI D08 series                                                    |
| **Depth (non-SEEG)**  | AdTech BF/RD/SD series, NeuroPace DL-344, NuMed Mini Lead                                                 |
| **Infusion**          | SmartFlow NGS-NC-06                                                                                       |
| **Research/Animal**   | ELAINE Rat, FHC WU Rat                                                                                    |


### 9.2 By `tipiscontact`


| tipiscontact=0                                                                                                                                                                                                                   | tipiscontact=1                                                                                          |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Medtronic 3387/3389/3391, BSci Vercise, all BSci Cartesia, Medtronic B33005/B33015, Abbott Directed 6172/6173, SceneRay (all), PINS (all), SDE-08 Legacy (all), NeuroPace, AdTech BF/RD/SD, NuMed, Aleva | BSci Vercise Directed†, Abbott ActiveTip (both), SDE-08 current (all), PMT (all), EPC (all), DIXI (all), SmartFlow, ELAINE, FHC |


† Boston Vercise Directed is the only *directed* lead with `tipiscontact=1`.

### 9.3 By Manufacturer


| Manufacturer              | Models                                                                                                 |
| ------------------------- | ------------------------------------------------------------------------------------------------------ |
| Medtronic                 | 3387, 3389, 3391, B33005, B33015                                                                       |
| Boston Scientific         | Vercise, Vercise Directed, Cartesia HX, Cartesia X                                                     |
| Abbott (St. Jude Medical) | ActiveTip 6146-6149, ActiveTip 6142-6145, Directed 6172, Directed 6173                                 |
| PINS Medical              | L301, L302, L303                                                                                       |
| SceneRay                  | SR1200, SR1202, SR1210, SR1211, SR1212, SR1242                                                         |
| Aleva Neurotherapeutics   | directSTIM Directed                                                                                    |
| SDE                       | S8/S10/S12/S14/S16 (legacy + current)                                                                  |
| PMT Medical               | 2102-{04,06,08,10,12,14,16}-091, -16-{092,093,131,142}                                                 |
| EPC                       | 2069-EPC-05C-35, 2069-EPC-15C-35                                                                       |
| NeuroPace                 | DL-344-3.5, DL-344-10                                                                                  |
| DIXI Medical              | D08-{05,08,10,12,15,18}AM                                                                              |
| AdTech                    | BF08R-SP05X/SP21X/SP61X, BF09R-SP61X-0BB, RD06R/RD08R/RD10R variants, SD06R/SD08R/SD10R/SD14R variants |
| SmartFlow                 | NGS-NC-06                                                                                              |
| ELAINE (research)         | Rat Electrode                                                                                          |
| FHC / WU                  | Rat Electrode                                                                                          |
| NuMed                     | Mini Lead                                                                                              |


---

## 10. Contact Coordinate Geometry — Formal Definition

For a lead aligned with the Z-axis with its tip at the origin, contact centres are computed as follows.

### 10.1 Ring Contacts (isdirected=0, or ring levels of directed leads)

```
z_k = tip_length * (1 − tipiscontact)
      + contact_length / 2
      + (k − 1) * eldist           for k = 1, 2, …, n_levels

where eldist = contact_spacing + contact_length   [uniform case]

contact_centre_k = [0, 0, z_k]
```

The first level's centre lies `tip_length + contact_length / 2` above the tip apex when the tip is insulating (`tipiscontact = 0`) and `tip_length / 2` above it when the tip is the first contact (`tipiscontact = 1`, the metal reaches the apex). Each further level centre adds the centre-to-centre pitch of its gap: `contact_length + contact_spacing[k−1]` when `contact_spacing` lists one gap per level gap, else `eldist`. See §7 for per-model values.

### 10.2 Segmented Contacts (directed leads)

At each directed axial level with z-centre `z_lev`, three segments are placed at:

```python
r  = lead_diameter / 2
cx = r * cos(π/6)   # = r * √3/2 ≈ 0.866 r
cy = r * sin(π/6)   # = r * 0.5

# Counter-clockwise convention (Boston Scientific, Medtronic):
seg_A = [  0,  +r,  z_lev ]   # 90° (Y+)
seg_B = [ -cx, -cy, z_lev ]   # 210°
seg_C = [ +cx, -cy, z_lev ]   # 330°

# Clockwise convention (Abbott Directed 6172 / 6173, Aleva directSTIM):
seg_A = [  0,  +r,  z_lev ]   # 90° (Y+)  — same
seg_B = [ +cx, -cy, z_lev ]   # 330°
seg_C = [ -cx, -cy, z_lev ]   # 210°
```

The segment centres are 120° apart; each segment covers `segment_arc_deg` (90°, 100° or 85°, §2.2) and the gaps between segments are insulation.

### 10.3 Non-uniform Contact Spacing

When `contact_spacing` is a vector, the z-position of contact k (1-based) is:

```
z_1 = 0 or tip_length  [depending on tipiscontact and model]
z_k = z_1 + contact_length + sum(contact_spacing[0:k-1])   for k > 1
```

For `eldist` only the second element of the spacing array is used (the uniform distal spacing after the non-standard first gap).

---

## 11. Mesh File Catalogue

Subdirectories exist under `templates/electrode_models/` for the following models. All others use procedurally-generated meshes (cylinder primitives, no mesh subdirectory).


| Model                              | Mesh Subdirectory                        | Format                |
| ---------------------------------- | ---------------------------------------- | --------------------- |
| Medtronic B33005                   | `Medtronic_B33005/`                      | TetGen `.smesh`/`.1`  |
| Medtronic B33015                   | `Medtronic_B33015/`                      | TetGen `.smesh`/`.1`  |
| Boston Scientific Vercise Directed | `Boston_Vercise_Directed/`               | TetGen `.smesh`/`.1`  |
| BSci Vercise Cartesia HX           | `Boston_Scientific_Vercise_Cartesia_HX/` | TetGen `.ele`/`.node` |
| BSci Vercise Cartesia X            | `Boston_Scientific_Vercise_Cartesia_X/`  | TetGen `.ele`/`.node` |
| Abbott Directed 6172               | `Abbott_Directed_05/`                    | TetGen `.smesh`/`.1`  |
| Abbott Directed 6173               | `Abbott_Directed_15/`                    | TetGen `.smesh`/`.1`  |
| PMT 2102-08-091                    | `PMT_2102-08-091/`                       | TetGen `.smesh`/`.1`  |
| PMT 2102-16-091                    | `PMT_2102-16-091/`                       | TetGen `.smesh`/`.1`  |
| PMT 2102-16-092                    | `PMT_2102-16-092/`                       | TetGen `.smesh`/`.1`  |
| PMT 2102-16-093                    | `PMT_2102-16-093/`                       | TetGen `.smesh`/`.1`  |
| PMT 2102-16-131                    | `PMT_2102-16-131/`                       | TetGen `.smesh`/`.1`  |
| PMT 2102-16-142                    | `PMT_2102-16-142/`                       | TetGen `.smesh`/`.1`  |
| Aleva directSTIM Directed          | `Aleva_directSTIM_Directed/`             | TetGen `.face`/`.1`   |
| SmartFlow NGS-NC-06                | `SmartFlow_NGS-NC-06/`                   | TetGen `.smesh`/`.1`  |
| ELAINE Rat Electrode               | `ELAINE_Rat_Electrode/`                  | TetGen `.smesh`/`.1`  |
| FHC WU Rat Electrode               | `FHC_WU_Rat_Electrode/`                  | TetGen `.smesh`/`.1`  |
| NuMed Mini Lead                    | `NuMedMiniLead/`                         | TetGen `.smesh`/`.1`  |


Additionally, a `LeadSketchUpModels/` subdirectory contains SketchUp source files (`.skp`) for some models — these are authoring assets, not runtime files.

---

## 12. Implementation Notes for Wyss DBS Localizer

### 12.1 Registry Architecture

```
dbsloc/electrodes/
    __init__.py
    spec.py          # ElectrodeSpec dataclass (§5)
    registry.py      # dict[str, ElectrodeSpec] + lookup functions
    models/
        medtronic.py
        boston_scientific.py
        abbott.py
        pins.py
        sceneray.py
        sde.py
        pmt.py
        dixi.py
        adtech.py
        neuropace.py
        research.py
        aleva.py
        smartflow.py
```

Each `models/*.py` module registers its electrodes into the global `REGISTRY: dict[str, ElectrodeSpec]`. The `matfname` string is the dict key.

### 12.2 Non-uniform Spacing

`contact_spacing` should be typed as `float | list[float]`. When it is a list, its length must equal `numContacts − 1`. The `eldist` formula (§2.1) uses `contact_spacing[1]` (0-indexed, i.e. the second gap) for the non-uniform case — this is what the MATLAB source does.

### 12.3 Mesh Loading

For Phase C, mesh files should be loaded from the original Lead-DBS installation path (or a re-distributed copy under a compatible licence — **check each mesh's licence before redistribution**). A future Rust FEM backend (no crate yet) should accept vertex/face arrays in f64/u32 format; use the `nifti` or a custom TetGen reader for the `.1.`* files.

### 12.4 Directed Contact Ordering

The `coords_mm` array in the existing `.mat` files stores contacts in the order they appear in `contactnames`. For directed leads this is:

- Ring contacts first (or as specified by `etageidx[0]`)
- Then segmented triplets in order (A, B, C for each level)
- Final ring last

Wyss DBS Localizer must preserve this ordering exactly, as `etageidx` indexes into it.

### 12.5 Contact Span vs. Lead Length

`contact_span` (§2.1) is the axial distance of the **active contact region** only. It does NOT include the lead shank above the topmost contact. Total lead length (from tip apex to connector) is not stored in `elspec` and varies by implant configuration.

---

## 13. Acceptance Criteria / Test Cases

For each model, the following must hold:

1. **Field completeness:** `ElectrodeSpec` for the model passes validation (all required fields set, types correct, lengths consistent).
2. **Computed field correctness:**
  - `eldist == contact_spacing + contact_length` (uniform) or `contact_spacing[1] + contact_length` (non-uniform)
  - `contact_span` matches formula in §2.1 to within 1e-9 mm
3. **Contact count consistency:**
  - `len(contactnames) == 2 * numContacts`
  - `sum(len(idx) for idx in etageidx) == numContacts`
  - `len(etagenames[0]) == len(etagenames[1]) == len(etageidx)`
4. **Directed geometry sanity:**
  - For directed leads: `len(etageidx)` levels where some `len(idx) == 3` exist
  - `markerpos` is not None for every model with `isdirected=True`; `markerlen` is not None for every such model except Aleva directSTIM (§2.2)
  - `segment_arc_deg` and the segment order of §2.2 hold for every directional model
5. **Round-trip lookup:** `get_elspec(elspec.elmodel).matfname == elspec.matfname`
6. **Catalogue size:** `len(list_models()) == 73`
7. **Specific numeric checks (spot tests):**


| Model                 | Field              | Expected value                                                         |
| --------------------- | ------------------ | ---------------------------------------------------------------------- |
| Medtronic 3389        | eldist             | 2.0 mm                                                                 |
| Medtronic 3389        | contact_span       | 7.5 mm                                                                 |
| Medtronic 3387        | contact_span       | 10.5 mm                                                                |
| Medtronic 3391        | contact_span       | 24.0 mm                                                                |
| Medtronic B33005      | contact_span       | 7.5 mm                                                                 |
| BSci Vercise Directed | contact_span       | 7.5 mm (tip contact 1.5 + 3 × 1.5 + 3 × 0.5)                           |
| BSci Vercise Directed | fiducial_span      | 6.0 mm                                                                 |
| BSci Cartesia HX      | contact_span       | 15.5 mm                                                                |
| BSci Cartesia HX      | fiducial_span      | 6.0 mm (tail at the 4th of 8 levels)                                   |
| BSci Cartesia X       | contact_span       | 11.5 mm                                                                |
| Abbott Directed 6172  | contact_span       | 7.5 mm                                                                 |
| PMT 2102-16-091       | eldist             | 3.5 mm                                                                 |
| PMT 2102-16-092       | eldist             | 3.97 mm                                                                |
| PMT 2102-16-142       | eldist             | 5.53 mm                                                                |
| NeuroPace DL-344-10   | contact_spacing    | 8.0 mm                                                                 |
| ELAINE Rat            | lead_diameter      | 0.225 mm                                                               |
| FHC WU Rat            | lead_diameter      | 0.125 mm                                                               |
| NuMed Mini Lead       | contact_span       | 3.5 mm                                                                 |
| NuMed Mini Lead       | fiducial_span      | 3.0 mm                                                                 |
| SceneRay SR1242       | contact_spacing    | [2.0, 4.0, 4.0] mm                                                     |
| AdTech BF08R-SP21X    | contact_spacing[0] | 1.43 mm                                                                |
| AdTech SD06R-SP26X    | contact_spacing[2] | 7.59 mm                                                                |


---

## 14. References

- Horn A et al. (2019). Lead-DBS v2: Towards a comprehensive pipeline for deep brain stimulation imaging. *NeuroImage* 184, 293–316.
- Pollo C et al. (2014). Directional deep brain stimulation: an intraoperative feasibility study. *Acta Neurochir* 156(7):1405–12.
- Dembek TA et al. (2019). Directional DBS increases side-effect thresholds — a prospective, double-blind trial. *Mov Disord* 34(8):1182–1187.
- MATLAB source (read-only reference): `<leaddbs>/templates/electrode_models/ea_resolve_elspec.m` and all `ea_elspec_*.m` files therein.

