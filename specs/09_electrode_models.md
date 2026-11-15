# Specification 09 — Electrode Models

**Status:** The catalogue of 73 lead models, its geometry rules (§2.2) and contact display names (§2.3) are implemented in Python (`dbsloc/electrodes/`) and embedded in Rust (`crates/dbsloc-reconstruct/src/electrode_catalog.rs`), except that the code does not yet follow the level spacing and tip-contact length of §10.1 / §11 for Abbott ActiveTip and SmartFlow (known deviation); display names in 2D slice export file names, reports and trajectory-planning level names are planned.
**Depends on:** none. Used by `05` (reconstruction), `06` (orientation), `07` (VTA), `08` (viewer), `10` (PaCER), `12` (`.mat` export), `14` (Lead Group) and `16` (session reports).

---

## 1. Purpose and scope

This specification is the contract for the electrode catalogue: the per-model data, where it lives, how contact geometry, model fiducials and contact names are derived from it, and the model coordinate frame that every consumer shares. It covers 73 models: deep brain stimulation (DBS) leads with ring or directional (segmented) contacts, stereo-electroencephalography (SEEG) and other depth electrodes, one infusion cannula and two rat electrodes.

Out of scope: fitting a model to an image (`05`, `10`), orientation detection (`06`), the volume conductor (`07`) and scene rendering beyond the lead body (`08`).

**Sides.** Side 1 is the right hemisphere and side 2 the left, the Lead-DBS order (Lead-DBS lists the right side's contact names first). Every per-side value in the catalogue code uses the keys `"1"` / `"2"` with this meaning.

**Provenance.** The per-model values of §6.1 and §7 were taken from the Lead-DBS electrode definitions (GPL-3.0, compatible with this GPL-3.0-only project); §2.2 records where they were checked against reference meshes and manufacturer data.

**Units.** Lengths in millimetres, angles in degrees.

---

## 2. Catalogue data model

### Data flow

The catalogue has one hand-edited source and two generated files. The derivation exists once, in Python; Rust embeds its result and never re-derives geometry from catalogue scalars.

| Step | File | Produced by | Drift guard |
| ---- | ---- | ----------- | ----------- |
| Source data | §6.1 of this file; marker values (`MARKERS`), level archetypes (`ARCH`) and the SmartFlow diameters (`build_rows`) in `scripts/emit_electrode_catalogue.py` | hand-edited | — |
| Catalogue | `dbsloc/electrodes/data/catalogue.json` (73 rows) | `scripts/emit_electrode_catalogue.py` | `tests/test_electrode_catalogue_export.py`, `--check` |
| Python model | `ElectrodeSpec`, `ElectrodeLayout`, display names (§5, §2.3) | `dbsloc/electrodes/archetypes.py` `row_to_spec`, `registry.py`, `layout.py` `layout_from_spec`, `names.py` | `tests/test_electrodes_registry.py`, `tests/test_electrode_names.py` |
| Embedded geometry | `crates/dbsloc-reconstruct/data/electrode_geometry.json` (schema 3, §2.4) | `scripts/emit_electrode_geometry.py` | `tests/test_electrode_geometry_export.py`, `--check` |
| Rust model | `ElectrodeGeometry`, embedded with `include_str!` | `crates/dbsloc-reconstruct/src/electrode_catalog.rs` | unit tests in that file |

Rules:

- To change a model, edit the source data, run `scripts/emit_electrode_catalogue.py` and then `scripts/emit_electrode_geometry.py`, and commit both outputs. A rule change in `archetypes.py`, `layout.py` or `names.py` needs the second emitter. Both emitters write LF line endings.
- The §6.1 table is machine-read. The emitter parses every Markdown table row of this file whose first cell is an integer and that has at least 11 cells, and requires exactly 73 such rows. Keep the column order of §6.1 and add no other table of that shape.

### Catalogue row fields (`catalogue.json`)

| Field | Type | Meaning |
| ----- | ---- | ------- |
| `ix` | int | Row number in §6.1 |
| `elmodel` | string | Display name: the primary lookup key (§4) and the name stored in files (§12) |
| `matfname` | string | Short identifier, the second lookup key |
| `numContacts` | int | Contacts per lead, each segment counted |
| `lead_diameter` | float | Shaft diameter; `r = lead_diameter / 2` is the radius of segment reference points and of the `x` / `y` fiducials |
| `contact_diameter`, `tip_diameter` | float | Equal to `lead_diameter` except SmartFlow (0.27 / 0.2); stored, not used |
| `tip_length` | float | Axial length of the tip beyond the most distal contact or, when `tipiscontact`, of the tip contact |
| `tipiscontact` | bool | The tip is the most distal contact; its metal reaches the apex |
| `contact_length` | float | Axial length of one contact (0 for the rat electrodes, whose only contact is the tip) |
| `contact_spacing` | float or list | Edge-to-edge gap between adjacent levels; a list must hold exactly one gap per level gap, starting at the most distal gap (§10.3) |
| `isdirected` | bool | The model has segmented levels |
| `forstimulation` | bool | Always `true`; stored, not used |
| `isdio_supported` | bool | Lead-DBS offers automatic orientation detection for the model: the §6.1 column DiO, forced to `true` for Boston Scientific Cartesia HX and X by the emitter (`_parse_table`); stored, not used; the orientation stage decides support itself (§8) |
| `markerpos` | float or null | Tip apex to orientation-marker centre (§2.2); required for directional models |
| `markerlen` | float or null | Axial length of the orientation marker; required for directional models except Aleva directSTIM (§2.2) |
| `archetype` | string | Level structure (table below) |

The archetype fixes the level index list `etageidx`: one tuple of 1-based contact indices per level, distal level first. Contacts are numbered consecutively level by level from the distal end and, within a directional level, in segment order A, B, C (§2.2). The archetype sets no length directly, but it fixes the level count `L`, which enters `contact_span`, `tail_level` (§2.1) and the trajectory factor (§2.2).

| `archetype` | Contacts per level, distal first | Models |
| ----------- | -------------------------------- | ------ |
| `ring1` | 1 | ELAINE and FHC WU rat electrodes |
| `ring4`, `ring4_tip` | 1, 1, 1, 1 | Medtronic 3387 / 3389 / 3391, SceneRay SR1242; Abbott ActiveTip (`ring4_tip`) |
| `ring8` | 1 × 8 | Boston Scientific Vercise |
| `seeg_<n>` | 1 × n | every other ring or depth model (n = `numContacts`) |
| `smart2` | 1, 1 | SmartFlow NGS-NC-06 |
| `m8d`, `b8d_tip`, `a8d` | 1, 3, 3, 1 | Medtronic B33005 / B33015, Boston Scientific Vercise Directed, Abbott Directed 6172 / 6173 |
| `a12d` | 3, 3, 3, 3 | Aleva directSTIM Directed |
| `c16_hx` | 3, 3, 3, 3, 1, 1, 1, 1 | Boston Scientific Vercise Cartesia HX |
| `c16_x` | 3, 3, 3, 3, 3, 1 | Boston Scientific Vercise Cartesia X |

`archetypes.py` `build_etage` also produces `contactnames` and `etagenames`: internal placeholder labels per archetype (for example `K1R`, `G0R`, `HR1`) used only for count checks. They are not display names and no output uses them (display names: §2.3).

### 2.1 Computed fields

`archetypes.py` `row_to_spec` computes, with `L = len(etageidx)` levels and `t = 1` when `tipiscontact`, else `0`:

```
eldist          = contact_spacing[1] + contact_length                       (list of ≥ 2 gaps)
                = contact_spacing + contact_length                          (scalar)
contact_span    = tip_length·t + contact_length·(L − t) + sum(contact_spacing)     (list)
                = tip_length·t + contact_length·(L − t) + contact_spacing·(L − 1)  (scalar)
tail_level      = L − 1   if L ≤ 4 or the model is SceneRay SR1202 / SR1212
                = 3       otherwise                                         (0-based level index)
segment_arc_deg = per-model value of §2.2 for directional models, none otherwise
```

- `eldist` is the nominal centre-to-centre level pitch. For a list it takes the second gap (the Lead-DBS rule), so it does not describe the first gap of the non-uniform depth leads; geometry and plausibility use per-gap pitches instead (§10.3). `ELDIST_OVERRIDE` in `archetypes.py` lists ten models whose values equal the formula; it only removes floating-point rounding.
- `contact_span` is the array span of §2.2. No model overrides it.
- `fiducial_span` is the model-frame depth of level `tail_level` (§2.2, §10.1). Python: `ElectrodeLayout.fiducial_span_mm`; Rust: `ElectrodeGeometry::fiducial_span_mm`.

### 2.2 Geometry rules (normative, 2026-11-14)

These rules were checked against the Lead-DBS catalogue, the Lead-DBS contact meshes and the manufacturers' published dimensions.

- **Array span (`contact_span`).** The axial length of the active array, from the distal edge of the most distal contact (the tip apex when the tip is a contact) to the proximal edge of the most proximal contact, computed over levels with the §2.1 formula. It equals the manufacturer's array span: Boston Scientific Vercise Directed 7.5 mm, Cartesia HX 15.5 mm, Cartesia X 11.5 mm, Medtronic 3389 / B33005 and Abbott 6172 7.5 mm, NuMed Mini Lead 3.5 mm. No model overrides it. It is used only for the trajectory extension and never as the head-to-tail distance: a trajectory is 50 evenly spaced points from `head − 5·û` to `tail + k·contact_span·û`, `û = (tail − head)/|tail − head|`, with `k = 1.5` when the model has more than 8 levels and `k = 2` otherwise (`electrode_catalog.rs` `trajectory`, `trajectory_extension_factor`; `05` §5, `12` §4).
- **Model fiducials.** In the model frame (§3.1) the **head** fiducial is the centre of the most distal level. The **tail** fiducial is the centre of the **4th level** counted from the distal end, or of the last level when the model has fewer than 4 levels; SceneRay SR1202 and SR1212 use their 8th contact (their last level). The `x` / `y` fiducials lie at radius `r = lead_diameter / 2` along model +x / +y at the head depth (`electrode_catalog.rs` `model_fiducials`).
- **Fiducial span (`fiducial_span`).** The tail fiducial depth minus the head fiducial depth: the first three level pitches for models with 4 or more levels (6.0 mm at a 2.0 mm pitch, for example Boston Scientific Vercise, Vercise Directed, Cartesia HX and X; 9.0 mm at 3.0 mm; 10.5 mm for the 3.5 mm SEEG pitch; 3.0 mm for NuMed Mini Lead), except SceneRay SR1202 / SR1212, whose tail at the 8th level gives seven pitches (14.0 / 21.0 mm); the single pitch of the two-level SmartFlow; 0 for a single-level model. It is the expected `|tail − head|`: reconstruction plausibility, TRAC/CORE tail placement, the axial stretch `s` of `05` §6.3, every model-to-marker fit, and the stretch of the viewer and volume-conductor lead bodies (§11) use it. Reconstruction files written before 2026-11-14 placed the tail at the last level; `crates/dbsloc-reconstruct/src/tail_fiducial.rs` `convert_legacy_tail` converts them on read (`05` §8).
- **Segment order.** On every directional level the first segment (A) lies at model +y, which is the orientation-marker direction of Boston Scientific and Abbott leads and the zero of the roll angle. Viewed from the proximal end (model +x to the right, +y up), segments B and C follow at +120° and +240° (counter-clockwise: B at 210°, C at 330° measured from +x) for Boston Scientific and Medtronic leads, and at −120° and −240° (clockwise: B at 330°, C at 210°) for Abbott Directed 6172 / 6173 and Aleva directSTIM (`layout.py` `_CLOCKWISE_SEGMENTS`). Medtronic's distal marker piece faces segment B, not A (`06` §10.9).
- **Segment arc (`segment_arc_deg`).** Each segment of a directional level covers `segment_arc_deg` of the circumference, centred on its azimuth; the rest of the level is insulation. Values measured on the reference meshes: 90° (Boston Scientific Vercise Directed, Medtronic B33005 / B33015, Abbott 6172 / 6173), 100° (Boston Scientific Cartesia HX / X), 85° (Aleva directSTIM) (`archetypes.py` `SEGMENT_ARC_DEG`, default 90°). The viewer and the volume conductor (`07`) use the same catalogue value.
- **Marker position.** `markerpos` is measured from the tip apex to the centre of the orientation marker. Marker centres in the reference meshes: 11 mm Boston Scientific Vercise Directed, 14.4 mm Medtronic B33005 (two pieces, 12.0–14.3 mm and 14.5–16.8 mm), 10.75 mm Abbott 6172, 20 mm Cartesia HX, 16 mm Cartesia X. Measured from the head fiducial the marker centre lies `markerpos − z_1` proximal (`z_1` of §10.1; `electrode_catalog.rs` `marker_offset_mm`): 10.25 mm Vercise Directed, 18.15 mm Cartesia HX, 14.15 mm Cartesia X, 9.0 / 12.0 mm Abbott 6172 / 6173, 12.75 / 15.75 mm Medtronic B33005 / B33015.
- **Aleva directSTIM.** Lead and tip diameter 1.35 mm with a rounded cap (the Lead-DBS tip diameter of 0.5 mm describes only the flat apex of that cap); no orientation-marker geometry; `markerpos` 13 mm is unverified and `markerlen` is not defined (null, `archetypes.py` `MARKERLEN_UNDEFINED`); automatic orientation detection (`06`) is not supported.
- **Medtronic B33005 / B33015.** The manufacturer's manual gives a lead diameter of 1.36 mm and a distal tip of 1.0 mm; the catalogue keeps the Lead-DBS values 1.3 mm and 0.9 mm for parity with existing reconstructions (known deviation).

### 2.3 Contact display names (normative, 2026-11-14)

Contact display names follow Lead-DBS. A name is `K` + a number (+ the segment letter `A` / `B` / `C` for Medtronic and Abbott directional levels), numbered distal to proximal from the per-model start of the side in the table below, numbers contiguous within a side. The side suffix is ` (R)` / ` (L)` in labels and is dropped or compacted (`K0R`) in file names. Level names: the ring contact's name; for Boston Scientific and Aleva directional levels `K<first>-<last>` with the plain number after the dash (`K10-12`; their segments carry plain numbers); for Medtronic and Abbott directional levels `K` + the level number (`K1`, `K2`). Segment letters follow the level's contact order (§2.2: `A` at +y, then the device's segment order). Stimulation settings, VTA files, the reconstruction file and group data stay positional (contacts 1..N per side) and never store names.

| Models | Right start | Left start | Style |
| ------ | ----------- | ---------- | ----- |
| Medtronic 3387 / 3389 / 3391, PINS L301 / L302 / L303, NeuroPace (both) | K0 | K8 | numeric |
| Medtronic B33005 / B33015 | K0 | K0 | level + letter (K0, K1A–C, K2A–C, K3) |
| Boston Scientific Vercise, Vercise Directed | K9 | K1 | numeric (segments numbered) |
| Boston Scientific Cartesia HX / X | K17 | K1 | numeric (segments numbered) |
| Abbott ActiveTip (both) | K1 | K1 | numeric |
| Abbott Directed 6172 / 6173 | K1 | K1 | level + letter (K1, K2A–C, K3A–C, K4) |
| SceneRay SR1200 / SR1210 / SR1211 / SR1242 | K4 | K0 | numeric |
| SceneRay SR1202 / SR1212 | K8 | K0 | numeric |
| Aleva directSTIM Directed | K1 | K13 | numeric (segments numbered) |
| NuMed Mini Lead, ELAINE rat, FHC WU rat | K0 | K0 | numeric |
| SmartFlow NGS-NC-06 | K0 (K0, K1) | K3 (K3, K4) | numeric (the Lead-DBS gap is kept) |
| SEEG / depth leads (SDE, PMT, EPC, DIXI, AdTech) | K0 | see below | numeric |

Left start of SEEG / depth leads (each SDE-08 size has one start for its current and Legacy entries): SDE-08 S8 8, S10 10, S12 12, S14 16, S16 16; PMT -04 4, -06 6, -08 8, -10 10, -12 12, -14 14, every -16 16; EPC 05C 8, 15C 16; DIXI 05 5, 08 8, 10 10, 12 12, 15 15, 18 18; AdTech BF08 8, BF09 9, RD06 6, RD08 8, every RD10 10, SD06 6, SD08 8, SD10 (both) 10, SD14 14.

Implementation:

- `dbsloc/electrodes/names.py`: `NAMING_RULES` (start per side and style per `matfname`), `contact_names(spec, side)` (contact index order) and `level_names(spec, side)`, side `"1"` or `"2"`. A model without a rule raises `UnspecifiedNaming` with `'<elmodel>' (<matfname>) has no contact naming rule in specs/09 §2.3`; a level-letter level that is neither a ring nor three segments raises `UnspecifiedNaming` too.
- The names are emitted into `electrode_geometry.json`; `electrode_catalog.rs` `contact_names(side)` / `level_names(side)` return them (an unknown side gives an empty list).
- The 3D viewer labels contacts `<name> (R)` / `<name> (L)` and numbers them by position (`R1…`, `L1…`) when the model is not catalogued (`crates/dbsloc-viz/src/scene3d_gpu.rs` `contact_labels`).
- **Planned:** names in 2D slice export file names (compact form `K0R`), reports and trajectory-planning level names.

### 2.4 Embedded geometry (`electrode_geometry.json`, schema 3)

Top-level keys: `schema` (3), `generator`, `source`, `unsupported` (models `layout_from_spec` rejected, each with a reason; must be empty) and `models`, one record per model sorted by display name. Numbers are rounded to 6 decimals.

| Record field | Meaning |
| ------------ | ------- |
| `elmodel`, `matfname`, `n_contacts`, `lead_diameter_mm`, `contact_length_mm`, `tip_length_mm`, `tip_is_contact`, `is_directed`, `marker_pos_mm`, `marker_len_mm` | Catalogue values (§2) |
| `eldist_mm`, `array_span_mm`, `tail_level`, `segment_arc_deg` | Computed fields (§2.1); `tail_level` 0-based, `segment_arc_deg` null for non-directional models |
| `level_z_mm` | Level centre depths in the model frame (§3.1, §10.1); `level_z_mm[0] = 0` |
| `contact_level` | 0-based level of each contact, in contact index order |
| `contact_xyz_mm` | Reference point of each contact in the model frame (§3.2, §10) |
| `name_style`, `name_start` | `numeric` or `level_letter`; start number per side (§2.3) |
| `contact_names`, `level_names` | Display names per side `"1"` / `"2"` (§2.3) |

`electrode_catalog.rs` `parse_catalog` validates the file on first use: per model, `contact_level` and `contact_xyz_mm` have `n_contacts` entries, `tail_level` is a valid level, `segment_arc_deg` lies strictly between 0° and 120°, and each side has one name per contact and per level. If any check fails, the catalogue is empty and every lookup fails. Derived accessors of `ElectrodeGeometry`:

| Accessor | Meaning |
| -------- | ------- |
| `n_levels`, `tail_level`, `array_span_mm`, `fiducial_span_mm`, `last_level_span_mm`, `segment_arc_deg` | §2.1, §2.2; `last_level_span_mm` is the depth of the last level |
| `apex_to_first_level_mm`, `apex_to_level_mm`, `marker_offset_mm` | `z_1`, level depths from the apex, marker offset from the head (§10.1, §2.2) |
| `model_fiducials` | `head`, `tail`, `x`, `y` in the model frame (§2.2) |
| `trajectory_extension_factor`, `trajectory_extension_mm`, `trajectory` | Trajectory rule (§2.2) |
| `contact_names`, `level_names` | §2.3 |

---

## 3. Model frame

### 3.1 Local frame

- The lead axis is the z axis; **+z points proximally**, from the tip toward the connector. The frame is right-handed; x and y are radial.
- The **origin is the centre of the most distal level**, which is the head fiducial: `level_z_mm[0] = 0`, and level k lies at depth `z(k)` (§10.1).
- **+y is the direction of segment A**; the roll angle is measured from it (`06`). How the orientation marker relates to the segments is per model (§2.2). The `x` / `y` fiducials are `(r, 0, 0)` and `(0, r, 0)`.
- The **tip apex** lies at `z = −z_1`, with `z_1 = tip_length / 2` when the tip is a contact and `tip_length + contact_length / 2` otherwise (§10.1). Distances "from the apex" (`markerpos`, the PaCER depths `apex_to_level_mm`) are `z + z_1`. The proximal edge of the metal lies at `z(L) + contact_length / 2` (`trac.rs` `apex_to_proximal_metal_mm` measures it from the apex).

Lead-DBS's electrode templates put `z = 0` at the tip apex. For most models the two frames differ by the constant `z_1` along z, which cancels in every marker fit (§12). Exceptions where Lead-DBS's template positions differ from this catalogue beyond that shift: Aleva directSTIM (0.03 mm, §7.13), SmartFlow NGS-NC-06 (§7.14) and the rat electrodes (§7.15).

### 3.2 Contact reference points

- Contacts are listed in contact index order, which is the row order of `contact_xyz_mm` and of every per-contact array (`coords_mm` included): level by level from the distal end and, within a directional level, segments A, B, C (§2).
- A ring contact (a level with one contact) has its reference point on the axis, `(0, 0, z(k))`.
- A segment has its reference point on the shaft surface at the segment's centre azimuth φ: `(r·cos φ, r·sin φ, z(k))`, `r = lead_diameter / 2`, φ from §10.2. It is a reference point, not the centroid of the segment's surface.
- `layout.py` rejects a level with any other contact count (`UnsupportedElectrodeLayout`: `'<elmodel>': level <k> has <n> contacts; specs/09 §3.2 defines radial placement for 1 (ring) and 3 (segmented) only`); no catalogue model has one.

| Segment | Counter-clockwise models (Boston Scientific, Medtronic) | Clockwise models (Abbott Directed 6172 / 6173, Aleva directSTIM) |
| ------- | ------------------------------------------------------- | ---------------------------------------------------------------- |
| A | 90°, `(0, r)` | 90°, `(0, r)` |
| B | 210°, `(−r·√3/2, −r/2)` | 330°, `(r·√3/2, −r/2)` |
| C | 330°, `(r·√3/2, −r/2)` | 210°, `(−r·√3/2, −r/2)` |

---

## 4. Lookup

### 4.1 Keys and matching

- A model is found by its display name (`elmodel`) or its `matfname`.
- Python `get_elspec(name)` matches either key exactly (case-sensitive) and raises `KeyError(name)` otherwise.
- Rust `by_name(name)` (re-exported as `dbsloc_reconstruct::electrode_by_name`) trims surrounding whitespace and compares ASCII case-insensitively; an empty name finds nothing.
- There are no aliases. Lead-DBS rewrites the former manufacturer name `St. Jude` to `Abbott` when it loads a reconstruction (`12` §3.1); this lookup does not, so such a name does not resolve.

### 4.2 No default lead

Neither path substitutes a model when none is given or the name is unknown. Contact positions and every plausibility tolerance derive from the model, and a stand-in lead (formerly a 1.5 mm-pitch four-contact lead that matches no device) gives wrong reconstructions without any signal.

- **Reconstruct stage** (`crates/dbsloc-pipeline/src/stage_impl.rs` and `ct_recon.rs`, `fn resolve_electrode`). The model comes from `RunOptions.el_model` (CLI `--el-model`, GUI reconstruct panel), else from the `elmodel` variable of the subject's `prefs/sub-<id>_desc-uiprefs.mat` (`uiprefs.rs`; logged as `dbsloc: note: reconstruct: electrode model from <path>: <model>`). Without a model the stage fails with ``no electrode model set; pass el_model in RunOptions (CLI `--el-model '<name>'`, GUI reconstruct panel) or set elmodel in the subject's prefs/sub-<id>_desc-uiprefs.mat``; a direct library call without that context reports `E_RECON_elmodel: no electrode model given; pass --el-model '<name>'`. An unknown name fails with `E_RECON_elmodel: "<name>" is not in the electrode catalogue (<n> known)`. Stage behaviour: `05`.
- **Orientation stage** (`orientation_stage.rs`). A side without a recorded model, or with an unknown one, gets status `failed` with `no electrode model is recorded for this side` or `electrode model "<name>" is not in the electrode catalogue` (`06`).
- **Viewer.** A side whose model does not resolve gets no lead body and positional contact labels (§2.3, §11).

### 4.3 The one remaining default

The Lead Group writer stores `Medtronic 3389` in `M.elstruct(p).elmodel` when the subject's reconstruction records no model and the group sets no override, as Lead-DBS does (`crates/dbsloc-pipeline/src/group/lead_group_mat.rs`, `DEFAULT_ELMODEL`; `14` §2.2.2).

### 4.4 Interfaces

| Language | Interface |
| -------- | --------- |
| Python | `dbsloc.electrodes.get_elspec(name) -> ElectrodeSpec`; `list_models() -> list[tuple[str, str]]` (`(elmodel, matfname)`, sorted, 73 entries); `dbsloc.electrodes.layout.layout_from_spec(spec) -> ElectrodeLayout`; `dbsloc.electrodes.names.naming_rule`, `contact_names`, `level_names` |
| Rust | `dbsloc_reconstruct::electrode_by_name(name) -> Option<&'static ElectrodeGeometry>`; `dbsloc_reconstruct::electrode_model_names()` (display names in file order, which is alphabetical); `dbsloc_pipeline::electrode_model_names()`, a wrapper around it, feeds the GUI model list |

---

## 5. Python records

### 5.1 `ElectrodeSpec`

`dbsloc/electrodes/spec.py`, a frozen dataclass built by `archetypes.py` `row_to_spec` and cached by `registry.py`. Fields:

- catalogue values (§2, without `ix` and `archetype`): `elmodel`, `matfname`, `lead_diameter`, `tip_diameter`, `tip_length`, `tipiscontact`, `numContacts`, `contact_length`, `contact_diameter`, `contact_spacing` (`float | list[float]`), `isdirected`, `forstimulation`, `isdio_supported`, `markerpos` and `markerlen` (`float | None`);
- computed values (§2.1): `eldist`, `contact_span`, `tail_level`, `segment_arc_deg` (`float | None`);
- level structure: `etageidx` (tuple of 1-based contact-index tuples per level) and the placeholder `contactnames` / `etagenames` (§2).

`row_to_spec` raises `ValueError` with `directed model missing markerpos: '<matfname>'` or `directed model missing markerlen: '<matfname>'` (the latter not for Aleva directSTIM).

### 5.2 `ElectrodeLayout`

`dbsloc/electrodes/layout.py` `layout_from_spec(spec)` resolves an `ElectrodeSpec` to model-frame geometry: `level_z_mm`, `contact_level` (0-based), `contact_xyz_mm` (§3, §10), the scalar values of the spec under `_mm` names (`lead_diameter_mm`, `n_contacts`, `contact_length_mm`, `tip_length_mm`, `tip_is_contact`, `eldist_mm`, `is_directed`, `marker_pos_mm`, `marker_len_mm`, `array_span_mm`, `tail_level`, `segment_arc_deg`), and the properties `n_levels`, `level_pitches_mm` and `fiducial_span_mm`. It raises `UnsupportedElectrodeLayout` with `'<elmodel>': etageidx is empty`, `'<elmodel>': etageidx covers <list>, expected 1..<n>`, or the §3.2 message. `scripts/emit_electrode_geometry.py` writes one `ElectrodeLayout` per model into the embedded geometry (§2.4).

---

## 6. Catalogue

### 6.1 Summary table (all 73 models)

This table is the source of the catalogue (§2). Columns: **#** = `ix`; **n** = `numContacts`; **Ø** = lead diameter, also used as contact and tip diameter except for SmartFlow; **CL** = `contact_length`; **CS** = `contact_spacing` (edge-to-edge); **TL** = `tip_length`; **TIC** = `tipiscontact`; **Dir** = `isdirected`; **DiO** = `isdio_supported` (Lead-DBS orientation support, not used, §8). **Y** = yes, N = no. Lengths in mm. The emitter strips the footnote marks †, ‡ and §, expands `a/b/c` and `a/b/c/d` to lists, `a/b×n` to `a` followed by n copies of `b` and `a/b/c×n` to `a`, `b` followed by n copies of `c`, forces DiO to yes for Cartesia HX and X, and takes `markerpos`, `markerlen` and the archetype from its own tables.

| #   | Display name (`elmodel`)              | `matfname`                 | Manufacturer      | n   | Ø (mm) | CL (mm) | CS (mm)           | TL (mm) | TIC   | Dir   | DiO   |
| --- | ------------------------------------- | -------------------------- | ----------------- | --- | ------ | ------- | ----------------- | ------- | ----- | ----- | ----- |
| 1   | Medtronic 3389                        | medtronic_3389             | Medtronic         | 4   | 1.27   | 1.5     | 0.5               | 1.5     | N     | N     | N     |
| 2   | Medtronic 3387                        | medtronic_3387             | Medtronic         | 4   | 1.27   | 1.5     | 1.5               | 1.5     | N     | N     | N     |
| 3   | Medtronic 3391                        | medtronic_3391             | Medtronic         | 4   | 1.27   | 3.0     | 4.0               | 1.5     | N     | N     | N     |
| 4   | Medtronic B33005                      | medtronic_b33005           | Medtronic         | 8   | 1.3    | 1.5     | 0.5               | 0.9     | N     | **Y** | **Y** |
| 5   | Medtronic B33015                      | medtronic_b33015           | Medtronic         | 8   | 1.3    | 1.5     | 1.5               | 0.9     | N     | **Y** | **Y** |
| 6   | Boston Scientific Vercise             | boston_vercise             | Boston Scientific | 8   | 1.3    | 1.5     | 0.5               | 1.1     | N     | N     | N     |
| 7   | Boston Scientific Vercise Directed    | boston_vercise_directed    | Boston Scientific | 8   | 1.3    | 1.5     | 0.5               | 1.5     | **Y** | **Y** | **Y** |
| 8   | Boston Scientific Vercise Cartesia HX | boston_vercise_cartesia_hx | Boston Scientific | 16  | 1.3    | 1.5     | 0.5               | 1.1     | N     | **Y** | **Y** |
| 9   | Boston Scientific Vercise Cartesia X  | boston_vercise_cartesia_x  | Boston Scientific | 16  | 1.3    | 1.5     | 0.5               | 1.1     | N     | **Y** | **Y** |
| 10  | Abbott ActiveTip (6146-6149)          | abbott_activetip_2mm       | Abbott            | 4   | 1.4    | 1.5     | 0.5               | 3.0     | **Y** | N     | N     |
| 11  | Abbott ActiveTip (6142-6145)          | abbott_activetip_3mm       | Abbott            | 4   | 1.4    | 1.5     | 1.5               | 3.0     | **Y** | N     | N     |
| 12  | Abbott Directed 6172 (short)          | abbott_directed_05         | Abbott            | 8   | 1.27   | 1.5     | 0.5               | 1.0     | N     | **Y** | **Y** |
| 13  | Abbott Directed 6173 (long)           | abbott_directed_15         | Abbott            | 8   | 1.27   | 1.5     | 1.5               | 1.0     | N     | **Y** | **Y** |
| 14  | SceneRay SR1200                       | sceneray_sr1200            | SceneRay          | 4   | 1.27   | 1.5     | 0.5               | 1.5     | N     | N     | N     |
| 15  | SceneRay SR1202                       | sceneray_sr1202            | SceneRay          | 8   | 1.27   | 1.5     | 0.5               | 1.5     | N     | N     | N     |
| 16  | SceneRay SR1210                       | sceneray_sr1210            | SceneRay          | 4   | 1.27   | 1.5     | 1.5               | 1.5     | N     | N     | N     |
| 17  | SceneRay SR1211                       | sceneray_sr1211            | SceneRay          | 4   | 1.27   | 1.5     | 1.0               | 1.5     | N     | N     | N     |
| 18  | SceneRay SR1212                       | sceneray_sr1212            | SceneRay          | 8   | 1.27   | 1.5     | 1.5               | 1.5     | N     | N     | N     |
| 19  | SceneRay SR1242                       | sceneray_sr1242            | SceneRay          | 4   | 1.27   | 3.0     | 2.0/4.0/4.0†      | 1.5     | N     | N     | N     |
| 20  | PINS Medical L301                     | pins_l301                  | PINS Medical      | 4   | 1.3    | 1.5     | 0.5               | 1.5     | N     | N     | N     |
| 21  | PINS Medical L302                     | pins_l302                  | PINS Medical      | 4   | 1.3    | 1.5     | 1.5               | 1.5     | N     | N     | N     |
| 22  | PINS Medical L303                     | pins_l303                  | PINS Medical      | 4   | 1.3    | 3.0     | 4.0               | 1.5     | N     | N     | N     |
| 23  | SDE-08 S8 Legacy                      | sde_08_s8_legacy           | SDE               | 8   | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     |
| 24  | SDE-08 S10 Legacy                     | sde_08_s10_legacy          | SDE               | 10  | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     |
| 25  | SDE-08 S12 Legacy                     | sde_08_s12_legacy          | SDE               | 12  | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     |
| 26  | SDE-08 S16 Legacy                     | sde_08_s16_legacy          | SDE               | 16  | 0.8    | 2.0     | 1.5               | 1.5     | N     | N     | N     |
| 27  | SDE-08 S8                             | sde_08_s8                  | SDE               | 8   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 28  | SDE-08 S10                            | sde_08_s10                 | SDE               | 10  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 29  | SDE-08 S12                            | sde_08_s12                 | SDE               | 12  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 30  | SDE-08 S14                            | sde_08_s14                 | SDE               | 14  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 31  | SDE-08 S16                            | sde_08_s16                 | SDE               | 16  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 32  | PMT 2102-04-091                       | pmt_2102_04_091            | PMT               | 4   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 33  | PMT 2102-06-091                       | pmt_2102_06_091            | PMT               | 6   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 34  | PMT 2102-08-091                       | pmt_2102_08_091            | PMT               | 8   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 35  | PMT 2102-10-091                       | pmt_2102_10_091            | PMT               | 10  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 36  | PMT 2102-12-091                       | pmt_2102_12_091            | PMT               | 12  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 37  | PMT 2102-14-091                       | pmt_2102_14_091            | PMT               | 14  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 38  | PMT 2102-16-091                       | pmt_2102_16_091            | PMT               | 16  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 39  | PMT 2102-16-092                       | pmt_2102_16_092            | PMT               | 16  | 0.8    | 2.0     | 1.97              | 2.0     | **Y** | N     | N     |
| 40  | PMT 2102-16-093                       | pmt_2102_16_093            | PMT               | 16  | 0.8    | 2.0     | 2.43              | 2.0     | **Y** | N     | N     |
| 41  | PMT 2102-16-131                       | pmt_2102_16_131            | PMT               | 16  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 42  | PMT 2102-16-142                       | pmt_2102_16_142            | PMT               | 16  | 0.8    | 2.0     | 3.53              | 2.0     | **Y** | N     | N     |
| 43  | 2069-EPC-05C-35                       | epc_05c                    | EPC               | 5   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 44  | 2069-EPC-15C-35                       | epc_15c                    | EPC               | 15  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 45  | NeuroPace DL-344-3.5                  | neuropace_dl_344_35        | NeuroPace         | 4   | 1.27   | 2.0     | 1.5               | 1.1     | N     | N     | N     |
| 46  | NeuroPace DL-344-10                   | neuropace_dl_344_10        | NeuroPace         | 4   | 1.27   | 2.0     | 8.0               | 1.1     | N     | N     | N     |
| 47  | DIXI D08-05AM                         | dixi_d08_05am              | DIXI Medical      | 5   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 48  | DIXI D08-08AM                         | dixi_d08_08am              | DIXI Medical      | 8   | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 49  | DIXI D08-10AM                         | dixi_d08_10am              | DIXI Medical      | 10  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 50  | DIXI D08-12AM                         | dixi_d08_12am              | DIXI Medical      | 12  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 51  | DIXI D08-15AM                         | dixi_d08_15am              | DIXI Medical      | 15  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 52  | DIXI D08-18AM                         | dixi_d08_18am              | DIXI Medical      | 18  | 0.8    | 2.0     | 1.5               | 2.0     | **Y** | N     | N     |
| 53  | AdTech BF08R-SP05X                    | adtech_bf08r_sp05x         | AdTech            | 8   | 1.28   | 1.57    | 3.43              | 1.0     | N     | N     | N     |
| 54  | AdTech BF08R-SP21X                    | adtech_bf08r_sp21x         | AdTech            | 8   | 1.28   | 1.57    | 1.43/3.93×6‡      | 1.0     | N     | N     | N     |
| 55  | AdTech BF08R-SP61X                    | adtech_bf08r_sp61x         | AdTech            | 8   | 1.28   | 1.57    | 1.43/4.43×6‡      | 1.0     | N     | N     | N     |
| 56  | AdTech BF09R-SP61X-0BB                | adtech_bf09r_sp61x_0bb     | AdTech            | 9   | 1.28   | 1.57    | 1.43/4.43×7‡      | 1.0     | N     | N     | N     |
| 57  | AdTech RD06R-SP05X                    | adtech_rd06r_sp05x         | AdTech            | 6   | 0.86   | 2.29    | 2.71              | 2.0     | N     | N     | N     |
| 58  | AdTech RD08R-SP05X                    | adtech_rd08r_sp05x         | AdTech            | 8   | 0.86   | 2.29    | 2.71              | 2.0     | N     | N     | N     |
| 59  | AdTech RD10R-SP03X                    | adtech_rd10r_sp03x         | AdTech            | 10  | 0.86   | 2.29    | 0.71              | 2.0     | N     | N     | N     |
| 60  | AdTech RD10R-SP05X                    | adtech_rd10r_sp05x         | AdTech            | 10  | 0.86   | 2.29    | 2.71              | 2.0     | N     | N     | N     |
| 61  | AdTech RD10R-SP06X                    | adtech_rd10r_sp06x         | AdTech            | 10  | 0.86   | 2.29    | 3.71              | 2.0     | N     | N     | N     |
| 62  | AdTech RD10R-SP07X                    | adtech_rd10r_sp07x         | AdTech            | 10  | 0.86   | 2.29    | 4.71              | 2.0     | N     | N     | N     |
| 63  | AdTech RD10R-SP08X                    | adtech_rd10r_sp08x         | AdTech            | 10  | 0.86   | 2.29    | 5.71              | 2.0     | N     | N     | N     |
| 64  | AdTech SD06R-SP26X                    | adtech_sd06r_sp26x         | AdTech            | 6   | 1.12   | 2.41    | 2.59/2.59/7.59×3§ | 2.0     | N     | N     | N     |
| 65  | AdTech SD08R-SP05X                    | adtech_sd08r_sp05x         | AdTech            | 8   | 1.12   | 2.41    | 2.59              | 2.0     | N     | N     | N     |
| 66  | AdTech SD10R-SP05X                    | adtech_sd10r_sp05x         | AdTech            | 10  | 1.12   | 2.41    | 2.59              | 2.0     | N     | N     | N     |
| 67  | AdTech SD10R-SP05X Choi               | adtech_sd10r_sp05x_choi    | AdTech            | 10  | 1.1    | 2.4     | 2.4               | 1.0     | N     | N     | N     |
| 68  | AdTech SD14R-SP05X                    | adtech_sd14r_sp05x         | AdTech            | 14  | 1.12   | 2.41    | 2.59              | 2.0     | N     | N     | N     |
| 69  | ELAINE Rat Electrode                  | elaine_rat_electrode       | ELAINE (research) | 1   | 0.225  | 0.0     | 0.0               | 0.1125  | **Y** | N     | N     |
| 70  | FHC WU Rat Electrode                  | fhc_wu_rat_electrode       | FHC / WU          | 1   | 0.125  | 0.0     | 0.0               | 0.250   | **Y** | N     | N     |
| 71  | NuMed Mini Lead                       | numed_minilead             | NuMed             | 4   | 0.3125 | 0.5     | 0.5               | 0.5     | N     | N     | N     |
| 72  | Aleva directSTIM Directed             | aleva_directstim_directed  | Aleva             | 12  | 1.35   | 1.5     | 0.5               | 1.1     | N     | **Y** | N     |
| 73  | SmartFlow Cannula NGS-NC-06           | smartflow_ngs-nc-06        | SmartFlow         | 2   | 1.65†  | 10.0    | 0.0               | 3.0     | **Y** | N     | N     |

Footnotes:

- † SceneRay SR1242 (CS): `contact_spacing = [2.0, 4.0, 4.0]`, one gap per level gap.
- ‡ AdTech BF08R-SP21X: `[1.43, 3.93, 3.93, 3.93, 3.93, 3.93, 3.93]`; BF08R-SP61X: `[1.43, 4.43 × 6]`; BF09R-SP61X-0BB: `[1.43, 4.43 × 7]`.
- § AdTech SD06R-SP26X: `[2.59, 2.59, 7.59, 7.59, 7.59]`.
- † SmartFlow (Ø): `lead_diameter = 1.65` (cannula outer diameter), `contact_diameter = 0.27`, `tip_diameter = 0.2`; the emitter sets these three values.

---

## 7. Per-model notes

The scalar values are in §6.1. This section adds the level structure, marker and segment data, level centres measured from the apex (§10.1), and known deviations from Lead-DBS.

### 7.1 Medtronic 3387 / 3389 / 3391

Four rings, insulating 1.5 mm tip, Ø 1.27 mm. Pitch 3.0 / 2.0 / 7.0 mm, array span 10.5 / 7.5 / 24.0 mm, fiducial span 9.0 / 6.0 / 21.0 mm. Medtronic 3389 level centres from the apex: 2.25, 4.25, 6.25, 8.25 mm.

### 7.2 Medtronic B33005 / B33015 (SenSight)

- Levels ring, 3 segments, 3 segments, ring (contacts 1 | 2–4 | 5–7 | 8); insulating 0.9 mm tip; gap 0.5 / 1.5 mm, pitch 2.0 / 3.0 mm, array span 7.5 / 10.5 mm.
- Level centres from the apex: 1.65, 3.65, 5.65, 7.65 mm (B33005); 1.65, 4.65, 7.65, 10.65 mm (B33015).
- Segments counter-clockwise (A at +y, B at 210°, C at 330°), arc 90°.
- Two-piece marker: `markerpos` 14.4 / 17.4 mm, `markerlen` 2.3 mm; the distal piece faces segment B (§2.2).
- Diameter deviation from the manufacturer: §2.2. Automatic orientation detection: `unsupported_model` (§8).

### 7.3 Boston Scientific

- **Vercise:** 8 rings, insulating 1.1 mm tip, pitch 2.0 mm, array span 15.5 mm, fiducial span 6.0 mm.
- **Vercise Directed:** ring, 3 segments, 3 segments, ring; the 1.5 mm tip is the first contact; pitch 2.0 mm; level centres from the apex 0.75, 2.75, 4.75, 6.75 mm; marker 11 mm / 3 mm; counter-clockwise; arc 90°; array span 7.5 mm.
- **Vercise Cartesia HX:** four directional levels, then four rings (contacts 1–12 segmented, 13–16 rings); insulating 1.1 mm tip; level centres from the apex 1.85 + 2.0·(k − 1) mm; marker 20 mm / 3 mm; arc 100°; array span 15.5 mm; fiducial span 6.0 mm (tail at the 4th of 8 levels).
- **Vercise Cartesia X:** five directional levels, then one ring; insulating 1.1 mm tip; marker 16 mm / 3 mm; arc 100°; array span 11.5 mm; fiducial span 6.0 mm.

### 7.4 Abbott (formerly St. Jude Medical)

- **ActiveTip 6146-6149 / 6142-6145:** four rings, Ø 1.4 mm; the 3.0 mm tip is the first contact; contacts 1.5 mm, gaps 0.5 / 1.5 mm; array span 9.0 / 12.0 mm. Level spacing after the tip contact: §10.1, implementation status.
- **Directed 6172 (short) / 6173 (long):** ring, 3 segments, 3 segments, ring; Ø 1.27 mm; insulating 1.0 mm tip; pitch 2.0 / 3.0 mm; level centres from the apex 1.75, 3.75, 5.75, 7.75 / 1.75, 4.75, 7.75, 10.75 mm; segments clockwise (B at 330°, C at 210°); arc 90°; marker 10.75 / 13.75 mm, length 1.5 mm; array span 7.5 / 10.5 mm.

### 7.5 SceneRay

All Ø 1.27 mm with an insulating 1.5 mm tip and ring contacts. SR1200, SR1210, SR1211: 4 rings of 1.5 mm, gaps 0.5 / 1.5 / 1.0 mm. SR1202, SR1212: 8 rings of 1.5 mm, gaps 0.5 / 1.5 mm; the tail fiducial is the 8th level (fiducial span 14.0 / 21.0 mm). SR1242: 4 rings of 3.0 mm, gaps 2.0, 4.0, 4.0 mm.

### 7.6 PINS Medical L301 / L302 / L303

As Medtronic 3389 / 3387 / 3391 with Ø 1.3 mm.

### 7.7 SDE-08 (SEEG)

Ø 0.8 mm, contacts 2.0 mm, gap 1.5 mm (pitch 3.5 mm). Legacy models (S8, S10, S12, S16): insulating 1.5 mm tip. Current models (S8, S10, S12, S14, S16): the 2.0 mm tip is the first contact.

### 7.8 PMT 2102 (SEEG)

Ø 0.8 mm, contacts 2.0 mm, the 2.0 mm tip is the first contact. In `2102-NN-ddd`, NN is the contact count; the gap is 1.5 mm for -091 and -131, 1.97 mm for -092, 2.43 mm for -093 and 3.53 mm for -142. 2102-16-091 and 2102-16-131 share every catalogue value.

### 7.9 EPC

2069-EPC-05C-35 and 2069-EPC-15C-35: as PMT 2102, with 5 and 15 contacts.

### 7.10 NeuroPace DL-344-3.5 / DL-344-10

Four rings, Ø 1.27 mm, contacts 2.0 mm, insulating 1.1 mm tip, gaps 1.5 / 8.0 mm.

### 7.11 DIXI Medical D08-xxAM (SEEG)

As PMT 2102 (gap 1.5 mm), with 5, 8, 10, 12, 15 or 18 contacts.

### 7.12 AdTech

All with an insulating tip.

- **BF (Behnke-Fried):** Ø 1.28 mm, contacts 1.57 mm, tip 1.0 mm. BF08R-SP05X: uniform gap 3.43 mm. BF08R-SP21X, BF08R-SP61X, BF09R-SP61X-0BB: a 1.43 mm first gap, then 3.93 / 4.43 / 4.43 mm (§6.1 footnote ‡); BF09R-SP61X-0BB has 9 contacts.
- **RD:** Ø 0.86 mm, contacts 2.29 mm, tip 2.0 mm. The `SPxxX` code is the centre-to-centre pitch in mm (SP03X 3 mm, SP05X 5 mm, …, SP08X 8 mm), so the gap is the pitch minus 2.29 mm (0.71 … 5.71 mm).
- **SD:** Ø 1.12 mm, contacts 2.41 mm, tip 2.0 mm, gap 2.59 mm; SD06R-SP26X has the gaps of §6.1 footnote §. SD10R-SP05X Choi: Ø 1.1 mm, contacts 2.4 mm, gap 2.4 mm, tip 1.0 mm.

### 7.13 Aleva directSTIM Directed

- Four directional levels (contacts 1–3, 4–6, 7–9, 10–12), no ring; Ø 1.35 mm with a rounded cap (§2.2); insulating 1.1 mm tip; pitch 2.0 mm; array span 7.5 mm.
- Level centres from the apex: 1.85, 3.85, 5.85, 7.85 mm. Lead-DBS's electrode template puts them 0.03 mm more distal (1.82, 3.82, 5.82, 7.82 mm).
- Segments clockwise (B at 330°, C at 210°), arc 85°.
- No marker geometry: `markerpos` 13 mm unverified, `markerlen` null; automatic orientation detection unsupported (§8).

### 7.14 SmartFlow NGS-NC-06

An infusion cannula, not a stimulation lead. Two "contacts" stand for the 3.0 mm tip (a contact) and a 10.0 mm window above it with no gap; Ø 1.65 mm, contact Ø 0.27 mm, tip Ø 0.2 mm. Level centres from the apex: 1.5 mm and, by the rule of §10.1, 8.0 mm, where Lead-DBS's electrode template also puts the second contact; the implementation places it at 11.5 mm (§10.1, implementation status).

### 7.15 Research electrodes

- **ELAINE rat electrode** (Ø 0.225 mm, tip 0.1125 mm) and **FHC WU rat electrode** (Ø 0.125 mm, tip 0.25 mm): one contact, which is the tip (`contact_length = 0`, `contact_spacing = 0`); one level, fiducial span 0. The catalogue reference point lies `tip_length / 2` from the apex; Lead-DBS's electrode template puts it at `tip_length`. TRAC/CORE refuses single-level models with `<name> has a single contact level, so TRAC/CORE cannot place a head and a distinct tail on it` (`trac.rs` `require_levels`).
- **NuMed Mini Lead:** four rings, Ø 0.3125 mm, contacts 0.5 mm, gap 0.5 mm, insulating 0.5 mm tip; array span 3.5 mm, fiducial span 3.0 mm.

---

## 8. Orientation detection support

The orientation stage decides support from the model, not from `isdio_supported` (`crates/dbsloc-reconstruct/src/orientation.rs` `support`): a model is supported when it is directional, its display name starts with `Boston` or `Abbott` (case-insensitive), and both `markerpos` and `markerlen` are defined. Method and statuses: `06` §10.

| Models | Status |
| ------ | ------ |
| Boston Scientific Vercise Directed, Cartesia HX, Cartesia X; Abbott Directed 6172, 6173 | supported (method `diode_marker_v1`) |
| Medtronic B33005, B33015 | `unsupported_model` |
| Aleva directSTIM Directed | `unsupported_model` |
| every non-directional model | `not_directed` |

The catalogue flag `isdio_supported` records Lead-DBS's support; it is `true` for Medtronic B33005 / B33015, for which Lead-DBS has a Medtronic-specific method that this implementation does not provide (`06` §10.2).

---

## 9. Consumers

| Consumer | Uses |
| -------- | ---- |
| TRAC/CORE reconstruction (`crates/dbsloc-reconstruct/src/trac.rs`, `plausibility.rs`, `native_frame.rs`) | level depths, metal extent, fiducial span, per-gap pitches, trajectory |
| PaCER (`crates/dbsloc-pacer`) | level depths from the apex and tail level; its own prototype table (`10` §9) selects the placement geometry when no model is pinned and, with `10` §8.5 and §8.6, gives the zero point and the final model |
| Orientation (`orientation.rs`, `crates/dbsloc-pipeline/src/orientation_stage.rs`) | support rule, marker offset and length, lead radius (§8) |
| Manual correction, template contacts, `.mat` export (`crates/dbsloc-pipeline/src/manual_correction.rs`, `template_contacts.rs`, `reco_mat.rs`) | model fiducials, contact reference points, trajectory, lead radius |
| VTA (`crates/dbsloc-vat/src/geometry.rs`, `crates/dbsloc-pipeline/src/vat/`) | lead body, contact count, segment arcs; a fingerprint of the model geometry is part of the VTA cache record |
| Viewer (`crates/dbsloc-viz/src/electrode_mesh.rs`, `scene3d_gpu.rs`, group and refinement views) | lead body, contact labels |
| GUI (`crates/dbsloc-app`) | model list of the electrode picker |
| Lead Group (`crates/dbsloc-pipeline/src/group/`) | contacts rebuilt from markers, default model (§4.3) |
| Session reports (`16`) | contact count of the named model |

---

## 10. Contact geometry — formal definition

Notation: levels `k = 1 … L` from distal to proximal (the code counts from 0); `g_k` is the gap between levels k and k + 1 (`contact_spacing[k − 1]` when `contact_spacing` is a list, else the scalar); `CL = contact_length`, `TL = tip_length`, `t = tipiscontact`.

### 10.1 Level depths

The length of level k is `ℓ_1 = TL` when `t = 1`, else `CL`; `ℓ_k = CL` for k ≥ 2.

```
z_1      = TL / 2               when t = 1    (the tip contact reaches the apex)
         = TL + CL / 2          when t = 0
z(1)     = 0
z(k + 1) = z(k) + ℓ_k / 2 + g_k + ℓ_(k+1) / 2
```

`z_1` is the distance from the tip apex to the centre of the most distal level (`electrode_catalog.rs` `apex_to_first_level_mm`), `z(k)` the model-frame depth of level k (`level_z_mm`), and `z_1 + z(k)` its distance from the apex (`apex_to_level_mm`). With this rule the array span of §2.1 equals `z(L) + ℓ_1 / 2 + ℓ_L / 2`, and `g_k` stays the edge-to-edge gap. For every model with an insulating tip, and for tip-contact models with `TL = CL`, each step is `CL + g_k`, which equals `eldist` for uniform leads. This rule replaces the earlier wording of this section ("each further level centre adds `eldist`"), which disagreed with the array span after a tip contact whose length differs from `CL`; it also matches Lead-DBS's SmartFlow template (second contact 8.0 mm from the apex, §7.14).

Implementation status: `layout.py` `_level_pitches` steps by `CL + g_k` for every gap, also after a tip contact whose length differs from `CL`. This affects Abbott ActiveTip 6146-6149 and 6142-6145 (`TL` 3.0 mm, `CL` 1.5 mm: levels 2–4 lie 0.75 mm too far distal, on 6146-6149 level 2 overlaps the tip contact by 0.25 mm; fiducial span 6.0 / 9.0 mm instead of 6.75 / 9.75 mm) and SmartFlow NGS-NC-06 (`TL` 3.0 mm, `CL` 10.0 mm: second level at `z` = 10.0 mm instead of 6.5 mm, fiducial span 10.0 mm instead of 6.5 mm). The same depths feed `electrode_catalog.rs` `apex_to_level_mm` (the PaCER depths) and `trac.rs` `apex_to_proximal_metal_mm` (ActiveTip 6146-6149: 8.25 mm instead of 9.0 mm). The single-level rat electrodes are unaffected.

### 10.2 Segment azimuths and arcs

On a directional level the three contacts, in ascending contact index, are segments A, B, C (`j = 0, 1, 2`) with centre azimuths measured from model +x:

```
φ_j = 90° + j·120°     counter-clockwise models (Boston Scientific, Medtronic)
φ_j = 90° − j·120°     clockwise models (Abbott Directed 6172 / 6173, Aleva directSTIM)
```

Reference point: `(r·cos φ_j, r·sin φ_j, z(k))` (§3.2). Segment j occupies the azimuths `φ_j ± segment_arc_deg / 2` over the axial range `z(k) ± CL / 2`; the rest of the level is insulation, so adjacent segments are separated by `120° − segment_arc_deg` (30°, 20° or 35°).

### 10.3 Non-uniform spacing

A `contact_spacing` list must have exactly `L − 1` entries, one per gap, starting at the most distal gap; every list in the catalogue does. Each gap uses its own step in §10.1 (for example AdTech BF08R-SP21X levels at 0, 3.0, 8.5, 14.0, … 36.0 mm; SceneRay SR1242 at 0, 5.0, 12.0, 19.0 mm). For a list of any other length `layout.py` `_level_pitches` falls back to `eldist` for every gap instead of rejecting the row; that fallback only tolerates a malformed row and is not part of this contract. Reconstruction plausibility compares measured pitches with these per-gap pitches (`plausibility.rs` `PlausibilityLimits::for_electrode`, `expected_pitches_mm`).

---

## 11. Lead bodies and rendering

No Lead-DBS mesh file is read or shipped; the Lead-DBS meshes served only to measure the segment arcs and marker positions of §2.2. Both 3D consumers build the lead from the embedded geometry in the model frame, stretch it along z by `s = |tail − head| / fiducial_span` (1 when the span is zero or not finite), and place it with the reconstruction's `head` / `tail` / `x` / `y` markers.

**Viewer** (`crates/dbsloc-viz/src/electrode_mesh.rs` `build_electrode`):

- Shaft: a cylinder of radius `r` from the top of the tip to the most proximal trajectory sample, and at least `CL + 1` mm beyond the last level centre.
- Levels: level k occupies `z(k) ± ℓ_k / 2` (§10.1): a full band per ring level and, per segment, a pad covering `segment_arc_deg` (inner radius 0.8 `r`, outer 1.06 `r`). A tip contact therefore runs from the apex (`z = −z_1 = −ℓ_1 / 2`) to `+ℓ_1 / 2`, rounded at the apex.
- Tip: when the tip is not a contact, a rounded insulating cap from the apex (`z = −z_1`) to the distal edge of the first level, `−ℓ_1 / 2`.
- Implementation status: `build_electrode` uses `CL` for every level (`half = CL / 2`, cap from the apex to `max(−CL / 2, apex + 0.1 mm)`). Abbott ActiveTip: the tip metal spans −1.5 … +0.75 mm (2.25 mm instead of 3.0 mm). SmartFlow NGS-NC-06: the first band spans −5 … +5 mm although the apex lies at −1.5 mm.
- Colours: insulation RGB (0.20, 0.21, 0.24), metal (0.80, 0.82, 0.86). The orientation marker is not drawn.
- The group and refinement views draw one lead per model (`canonical_electrode`, 30 mm shaft) and place it with `lead_instance_matrix`.

**Volume conductor** (`crates/dbsloc-vat/src/geometry.rs` `LeadBody`): a cylinder of radius `r` with a rounded cap of length `min(s·TL, r)` at the apex; contact k occupies `s·z(k) ± s·ℓ_k / 2` (a tip contact from the apex); a segment occupies its arc in the outer half of the radius; everything else, the orientation marker included, is insulation. Details: `07` §3.1. Implementation status: `LeadBody::new` ends every contact at `s·(z(k) + CL / 2)`, also the tip contact (`z_hi = z + half`). Abbott ActiveTip: tip contact −1.5 … +0.75 mm instead of −1.5 … +1.5 mm; SmartFlow NGS-NC-06: −1.5 … +5.0 mm instead of −1.5 … +1.5 mm (at `s = 1`).

---

## 12. Lead-DBS interoperability

### 12.1 What files store

The only catalogue datum in Lead-DBS-format files is the display name: `reco.props(k).elmodel` in the reconstruction `.mat` (`12` §3.1), `elmodel` in `uiprefs.mat` (read by `crates/dbsloc-pipeline/src/uiprefs.rs`) and `M.elstruct(p).elmodel` in a Lead Group file (`14` §2.2.2). Writers store the §6.1 display name verbatim; readers resolve it with §4. Contact arrays (`coords_mm`, stimulation contact vectors, VTA files) are positional in contact index order (§2), side 1 = right and side 2 = left; no file stores contact names.

### 12.2 Field mapping

The catalogue uses the Lead-DBS `elspec` field names where the meaning is the same.

| Lead-DBS | Here | Note |
| -------- | ---- | ---- |
| `elmodel`, `matfname`, `numContacts`, `lead_diameter`, `contact_diameter`, `tip_diameter`, `tip_length`, `tipiscontact`, `contact_length`, `contact_spacing`, `isdirected`, `forstimulation`, `markerpos`, `markerlen`, `eldist`, `contact_span` | same names (catalogue row, `ElectrodeSpec`) | `contact_span` is the array span (§2.2) |
| `etageidx` | `ElectrodeSpec.etageidx`; Rust `contact_level` (0-based level per contact) | |
| `contactnames`, `etagenames` | `names.py` `contact_names` / `level_names` per side, plus the side suffix (§2.3) | the `ElectrodeSpec` fields of these names are placeholders |
| `contact_color`, `lead_color`, `tip_color` | not kept | viewer colours: §11 |
| electrode template `coords_mm` | `contact_xyz_mm` | Lead-DBS `z = 0` at the apex, here at the head: `z_here = z_LeadDBS − z_1`, except Aleva directSTIM, SmartFlow and the rat electrodes, whose Lead-DBS positions differ further (§3.1) |
| electrode template `head_position`, `tail_position`, `x_position`, `y_position` | `model_fiducials()` | same points, same shift |
| — | `isdio_supported`, `archetype`, `tail_level`, `segment_arc_deg`, `fiducial_span` | additions of this implementation |

---

## 13. Acceptance criteria

Tests: `tests/test_electrodes_registry.py`, `tests/test_electrode_catalogue_export.py`, `tests/test_electrode_geometry_export.py`, `tests/test_electrode_names.py`, `tests/test_reconstruction_io_tail.py`, and the Rust unit tests in `electrode_catalog.rs`, `tail_fiducial.rs`, `orientation.rs` (`support_follows_the_family_table`), `electrode_mesh.rs` and `plausibility.rs`.

1. **Catalogue:** 73 models; each resolves by display name and by `matfname`; `catalogue.json` and `electrode_geometry.json` equal a fresh emitter run; `unsupported` is empty.
2. **Level structure:** the levels cover contacts 1..N exactly once; every level has 1 or 3 contacts; every `contact_spacing` list has exactly `L − 1` entries; `level_z_mm[0] = 0`; each contact's z equals its level depth; rings lie on the axis and segments at radius `r`.
3. **Computed fields:** `eldist` and `contact_span` follow §2.1 within 1e-9 mm; `tail_level` follows §2.1.
4. **Directional models:** `markerpos` is set; `markerlen` is set except for Aleva directSTIM; `segment_arc_deg` and the segment order follow §2.2; a directional row without `markerlen` (other than Aleva) is rejected.
5. **Names:** every model has a naming rule; each side has one name per contact and per level.
6. **Spot values:**

| Model | Quantity | Expected |
| ----- | -------- | -------- |
| Medtronic 3389 | `eldist` / array span / fiducial span | 2.0 / 7.5 / 6.0 mm |
| Medtronic 3387 | `eldist` / array span / fiducial span | 3.0 / 10.5 / 9.0 mm |
| Medtronic 3391 | array span | 24.0 mm |
| Medtronic B33005 | array span / marker offset from the head | 7.5 / 12.75 mm |
| Boston Scientific Vercise | tail level / fiducial span | 3 / 6.0 mm |
| Boston Scientific Vercise Directed | array span / fiducial span / `z_1` / marker offset | 7.5 / 6.0 / 0.75 / 10.25 mm |
| Boston Scientific Cartesia HX | array span / fiducial span / last level depth / arc | 15.5 / 6.0 / 14.0 mm / 100° |
| Boston Scientific Cartesia X | array span / fiducial span / marker offset | 11.5 / 6.0 / 14.15 mm |
| Abbott Directed 6172 | array span / marker offset / azimuths A, B, C | 7.5 / 9.0 mm / 90°, 330°, 210° |
| Aleva directSTIM Directed | arc / `markerpos` / `markerlen` | 85° / 13.0 mm / null |
| SceneRay SR1202, SR1212 | tail level | 7 (8th level) |
| PMT 2102-16-091 / -092 / -142 | `eldist` | 3.5 / 3.97 / 5.53 mm |
| 2069-EPC-15C-35 | array span / trajectory factor | 51.0 mm / 1.5 |
| NuMed Mini Lead | array span / fiducial span | 3.5 / 3.0 mm |
| ELAINE rat, FHC WU rat | array span / `eldist` / tail level | 0.1125, 0.25 / 0 / 0 |
| SmartFlow NGS-NC-06 | right / left contact names | K0, K1 / K3, K4 |
| SceneRay SR1242 | `contact_spacing` | [2.0, 4.0, 4.0] mm |
| AdTech BF08R-SP21X | first two level pitches | 3.0 / 5.5 mm |
| AdTech SD06R-SP26X | `contact_spacing[2]` | 7.59 mm |

---

## 14. References

- Horn A et al. (2019). Lead-DBS v2: Towards a comprehensive pipeline for deep brain stimulation imaging. *NeuroImage* 184, 293–316.
- Pollo C et al. (2014). Directional deep brain stimulation: an intraoperative feasibility study. *Acta Neurochir* 156(7):1405–12.
- Dembek TA et al. (2019). Directional DBS increases side-effect thresholds — a prospective, double-blind trial. *Mov Disord* 34(8):1182–1187.
