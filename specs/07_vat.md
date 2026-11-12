# Spec 07 — Stimulation settings, E-field and volume of tissue activated (VTA) (Wyss DBS Localizer)

> Phase B specification, rewritten 2026-10-23 from observation of the reference implementation (Lead-DBS 3.2.1 per `.version.txt`, git `5b1008d`, 2026-03-02) and from the cited papers. No reference code is reproduced: behaviour is given as prose, equations, tables and file-format facts. Normative for the `vat` stage (`crates/dbsloc-pipeline`), the stimulation-settings model shared by the GUI, the CLI and the server, and the E-field engine. Referenced as `specs/07` §n. Group analyses that consume these outputs are specified in `specs/14_lead_group.md`; connectomes in `specs/15_connectomes.md`. Owner decisions of 2026-10-23: `specs/14_lead_group.md` §15.

**Status:** Phase B (2026-10-23). Supersedes the 97-line version of 2026-09.
**Depends on:** `00` (stage contract, locks, result tokens), `01` §7a (derivative root, naming), `04` §5.4 / §5.4a / §11 (transforms, GPU engine rules, bridge), `05` §6 / §8 (Reco v2 markers, contacts, side keys), `09` (electrode catalogue), `11` (atlas folders, `gm_mask.nii.gz`), `12` Part 1 (MAT v7 export pattern, provenance).

**Conventions.** Side 1 = right hemisphere = `R`, side 2 = left = `L` (`05` §8). World coordinates are RAS millimetres. "Native" = the coregistered anchor space (`space-anchorNative`, `00` §2); "template" = the normalization template space (`meta.template_space`, e.g. `MNI152NLin2009bAsym`). MATLAB classes are named as MATLAB names them. Contact index `k` is 1-based and follows the catalogue contact order of `09` (index 1 = most distal; segmented levels in the `09` §10.2 segment order), which is also the row order of Reco `coords_mm`. Facts that could not be confirmed are marked **unverified**.

**Owner decisions locked here (2026-10-23).** (D1) Both use cases from the start: research group analyses and single-patient programming support. (D2) E-field VTAs are required in the first release; the analytical sphere models stay as clearly labelled additional options. (D3) VTAs and E-fields are computed in **native** space and warped to template space; both are stored. (D4) Full multi-source (independent sources, e.g. Boston Vercise multiple independent current control, Abbott), bipolar / multipolar and case settings are in the data model and are computed; voltage and current mode. (D5) Maximal Lead-DBS compatibility: read Lead-DBS stimulation files, write Lead-DBS-compatible stimulation outputs. (D6) One implementation for desktop and GPU server, run as the pipeline stage `vat`. (D7) No OSS-DBS / conda dependency: in-process Rust, GPU via wgpu where natural.

---

## 1. Purpose and scope

For one subject, one **stimulation** (a labelled set of settings for up to two leads, up to four sources per lead) and the subject's reconstruction, produce per hemisphere:

- the **E-field magnitude** |E| (V/m) on a regular grid, in native and template space;
- the **binary VTA** = the region where |E| exceeds the activation threshold (analytical models: a sphere);
- a normalised weighting map (`efieldgauss`, §5.3), a display mesh, the volume;
- **atlas overlap statistics** (§8) in an `ea_stats`-compatible record;
- Lead-DBS-compatible files so Lead-DBS and Lead Group can read the results (§5).

### 1.1 Models

| `model` id (ours) | Lead-DBS display name (`S.model`) | Lead-DBS file slug | Class | Status in Wyss DBS Localizer |
| --- | --- | --- | --- | --- |
| `efield` | `SimBio/FieldTrip (see Horn 2017)` | `simbio` | Volume-conductor E-field (quasi-static, §3) | **Default.** Computed in process (§3). Replaces the reference FEM (FieldTrip/SimBio + iso2mesh + TetGen) with our own solver. |
| `kuncel` | `Kuncel 2008` | `kuncel` | Analytical sphere, voltage (§6.1) | Option, labelled "approximate sphere". |
| `maedler` | `Maedler 2012` | `maedler` | Analytical sphere, voltage + impedance (§6.2) | Option, labelled likewise. |
| `dembek` | `Dembek 2017` | `dembek` | Analytical sphere, current density threshold (§6.3) | Option, labelled likewise. |
| `fastfield` | `Fastfield (Baniasadi 2020)` | `fastfield` | Precomputed per-contact fields superposed | **Not computed.** The reference templates are GPL-distributed data. Settings files with this model are imported (their `model` kept); computing them asks for `efield`. Existing Lead-DBS outputs in a stimulation folder are read by group analyses like any other model's (`specs/14`). Our `efield` engine provides the same instant-update behaviour through its own per-contact basis (§3.6). |
| `ossdbs` | `OSS-DBS (Butenko 2020)` | `ossdbs` | External FEM (NGSolve), optional axon models | **Not computed** (D7). Existing OSS-DBS outputs in Lead-DBS folders (`…_model-ossdbs_hemi-<H>.nii`) are read as imported results, nothing else. |

The six display names and slugs are the only ones the reference accepts; any other `S.model` makes its model-to-slug lookup fail (verified). Therefore files we write for `efield` carry the display name `SimBio/FieldTrip (see Horn 2017)` and slug `simbio` (open decision O1, §11 notes), and our own engine identity is recorded in the sidecars (§5.4).

### 1.2 Out of scope

Pathway activation modelling (axon cable models, NEURON), DTI-based anisotropic conductivity, frequency-domain / dispersive tissue (Cole–Cole), electrode–tissue interface (double layer) models, the OSS-DBS optimiser and StimSets, the reference's tetrahedral meshing. Group-level batch, mirroring (`hemidesc-FlippedFromLeft` files), sweet-spot, fibre filtering and network mapping are `specs/14` / `specs/15`; this spec only provides the per-subject inputs they read.

---

## 2. Stimulation settings

### 2.1 Where Lead-DBS stores them

- Folder: `<derivatives root>/sub-<id>/stimulations/<space>/<label>/`, with `<space>` = `native` or the template space name (the reference's space name, `MNI152NLin2009bAsym` by default). Lead Group stimulations use the label `gs_<guid>`; the stimulation list hides folders whose name starts with `.` or `gs_` (verified). Default label of a new stimulation = the local time `yyyyMMddHHmmss`; user labels starting with `gs_` are refused by the reference dialog. Our editor follows that rule; its only exception is the group mode of `specs/14` §4.2 (amended 2026-10-25), which opens one existing or new `gs_<guid>` label of one patient.
- File: `sub-<id>_desc-stimparameters.mat`, one variable `S`, saved with MATLAB's default format (v7 Level-5 by factory default, v7.3 when the user changed the MATLAB preference; same rule as `12` §2). When a stimulation is computed in native space, the reference writes the same `S` into the native and the template folder (verified for the command-line wrapper and the FEM path).
- A JSON twin `sub-<id>_desc-stimparameters.json` (a generic MATLAB-to-JSON dump of `S`) is written into the label folder by the reference's external "programmer" integration only (verified). It is not a schema we adopt; we never write a file of that name (§2.3).
- The model parameters (threshold, conductivities, …) are **not** in `S`: they are machine preferences of the reference (`prefs.machine.vatsettings`, §2.5). A stimulation file therefore does not fully determine a reference result.

### 2.2 The `S` structure (version `S.ver = '2.0'`)

Every field found in the reference. "Per source" = inside `Rs<j>` / `Ls<j>`, j = 1..4.

| Field | Class / shape | Scope | Unit / range | Meaning |
| --- | --- | --- | --- | --- |
| `label` | char | stimulation | — | Folder name of the stimulation. |
| `Rs1` … `Rs4`, `Ls1` … `Ls4` | 1×1 struct each | per side × source | — | Up to four independent sources per side; all eight always present. |
| ↳ `k1` … `kN` | 1×1 struct each | per source × contact | N = `numContacts` | One per catalogue contact. |
| ↳↳ `perc` | double | per contact | 0–100 | Share of the source amplitude carried by this contact within its polarity group (§2.4). 0 = not used. |
| ↳↳ `pol` | double | per contact | 0 off, 1 cathode (negative), 2 anode (positive) | Polarity. |
| ↳↳ `imp` | double | per contact | kΩ, default 1 | Contact impedance (used by `maedler`, `dembek`, FastField). |
| ↳ `case` | struct (`perc`, `pol`) | per source | defaults `perc` 100, `pol` 2 | The implantable pulse generator housing as a return electrode. |
| ↳ `amp` | double | per source | V (voltage mode) or mA (current mode), default 0 | Source amplitude. |
| ↳ `va` | double | per source | 1 = constant voltage, 2 = constant current (default 2) | Control mode. |
| ↳ `pulseWidth` | double | per source | µs, default 60 | Pulse width; read only by OSS-DBS in the reference. |
| `active` | 1×2 double | per side | 1–4, default [1 1] | Source shown in the reference GUI per side (bookkeeping). |
| `model` | char | stimulation | one of the six names of §1.1 (default `SimBio/FieldTrip (see Horn 2017)`) | VTA model. |
| `monopolarmodel` | double | stimulation | 0/1 | 1 for Kuncel, Mädler, Dembek (GUI enforces one contact per source), else 0. |
| `amplitude` | 1×2 cell of 1×4 double | per side × source | as `amp` | Duplicate of the eight `amp` values. |
| `activecontacts` | 1×2 cell of 1×N double (0/1) | per side × contact | — | Derived: contact used by any source with `perc > 0` and `amp > 0`. |
| `numContacts` | double | stimulation | catalogue `numContacts` | One value for both sides. |
| `sources` | 1×4 double | stimulation | `[1 2 3 4]` | Sources to compute. |
| `volume` | 1×2 double | per side | mm³ | VTA volume written back by FEM / FastField runs (§5.3). |
| `ver` | char | stimulation | `'2.0'` | Format version. |
| `template`, `estimateInTemplate` | double | stimulation | 0/1 | Only in files that passed through the programmer integration (the latter is removed again before saving, **unverified** for every path). |

There is **no frequency field** in `S` (verified by search). There is no threshold, conductivity or encapsulation in `S`.

**Legacy files (no `ver`).** Before version 2.0 each source held `k0` … `k15`: `k0`–`k7` were the right lead's contacts and `k8`–`k15` the left lead's. The reference converts on load: right contact n ← `k(n−1)`, left contact n ← `k(n+7)`, for n = 1..min(N, 8), keeps `case`, `amp`, `va`, `pulseWidth`, sets `ver = '2.0'` and re-saves (verified). Fields `sources`, `volume`, `numContacts` are added when missing (`numContacts` from the electrode model).

**Contact count mismatch.** When an existing `S` has fewer `k` fields than the current model, the reference appends `perc 0, pol 0, imp 1` contacts; when it has more, it removes the excess fields (verified).

### 2.3 Our schema: `sub-<id>_desc-stimulation.json`

The canonical settings file of a stimulation is `stimulations/native/<label>/sub-<id>_desc-stimulation.json` (schema name `dbsloc-stimulation`, version 1). The MAT file `sub-<id>_desc-stimparameters.mat` is derived from it on every write (§5.2) and is read only on import. Example (one source shown):

```json
{
  "schema": "dbsloc-stimulation",
  "schema_version": 1,
  "label": "20261023101500",
  "subject": "01",
  "model": "efield",
  "model_settings": {
    "threshold_v_per_mm": 0.2,
    "conductivity": { "mode": "gm_wm", "gm_s_per_m": 0.33, "wm_s_per_m": 0.14, "homogeneous_s_per_m": null, "gm_mask": "atlas:DISTAL Minimal (Ewert 2017)" },
    "encapsulation": { "thickness_mm": 0.0, "s_per_m": null },
    "remove_electrode": true,
    "source_combination": "max"
  },
  "sides": {
    "right": {
      "active_source": 1,
      "sources": [
        { "index": 1, "mode": "current", "amplitude": 2.5, "pulse_width_us": 60, "frequency_hz": 130,
          "case": { "polarity": "anode", "percent": 100 },
          "contacts": [ { "index": 1, "polarity": "off", "percent": 0, "impedance_kohm": 1 },
                        { "index": 2, "polarity": "cathode", "percent": 100, "impedance_kohm": 1 } ] }
      ]
    },
    "left": { "active_source": 1, "sources": [] }
  },
  "lead_dbs": { "ver": "2.0", "extra_fields": {} },
  "provenance": { "software": { "product": "dbsloc", "version": "…", "git_hash": "…" }, "written_at": "2026-10-23T10:15:00Z", "imported_from": null }
}
```

Rules: `sides.right.sources` and `sides.left.sources` hold exactly four entries (index 1..4) after normalisation; a writer may omit trailing all-zero sources, a reader fills them with defaults. `contacts` holds exactly N entries for the side's electrode model (N from `09`); `polarity` ∈ {`off`, `cathode`, `anode`}; `mode` ∈ {`voltage`, `current`}; `amplitude` is in V (voltage) or mA (current). `frequency_hz` is ours only (informational; no model reads it; not written to `S`). `model_settings` holds every parameter that changes the result (§2.5); missing keys take the §2.5 defaults and the resolved values are recorded in the results sidecar.

### 2.4 Mapping `S` ↔ JSON

| `S` | JSON | Rule |
| --- | --- | --- |
| `label` | `label` | Same string. |
| `model` | `model` | Display name ↔ id per §1.1. Unknown name: import refused, `E_VAT_model`. |
| `R`/`L` | `sides.right` / `sides.left` | Side 1 = R = right. |
| `Rs<j>` / `Ls<j>` | `sides.<side>.sources[j−1]` | j = 1..4. |
| `…k<n>.pol` | `contacts[n−1].polarity` | 0 ↔ `off`, 1 ↔ `cathode`, 2 ↔ `anode`. Other values: `E_VAT_settings`. |
| `…k<n>.perc` | `contacts[n−1].percent` | Double, unchanged. |
| `…k<n>.imp` | `contacts[n−1].impedance_kohm` | Double; missing → 1 with a note. |
| `…case.pol` / `.perc` | `case.polarity` / `case.percent` | As contacts. |
| `…amp` | `amplitude` | Unchanged. |
| `…va` | `mode` | 1 ↔ `voltage`, 2 ↔ `current`. |
| `…pulseWidth` | `pulse_width_us` | Missing → 60 with a note. |
| `active` | `active_source` per side | Unchanged. |
| `amplitude` | — | Derived on export from the `amp` values. On import, a value that differs from the source's `amp` is reported (`dbsloc: warning: vat: <label>: S.amplitude disagrees with Rs<j>.amp; using amp`); `amp` wins because every reference model except OSS-DBS reads `amp`. |
| `activecontacts` | — | Derived on export (§2.2 rule). |
| `numContacts` | — | Export: N of the side's model; when the two sides use models with different N, the larger N, the shorter side padded with `perc 0, pol 0, imp 1` (warning; the reference cannot represent two different models). |
| `monopolarmodel` | — | Derived: 1 for `kuncel`/`maedler`/`dembek`, else 0. |
| `sources` | — | Export `[1 2 3 4]`; import ignores it (all sources are computed). |
| `volume` | — | Export: the raw VTA volume per side of the last computation (§5.3), `[0 0]` before one. |
| `ver` | `lead_dbs.ver` | Export `'2.0'`. |
| any other field (`template`, …) | `lead_dbs.extra_fields` | Kept verbatim (numbers, strings, logical, nested structs/cells as JSON), written back on export, so `S → JSON → S` is lossless for every field. |

**Round trip.** JSON → `S` → JSON is identical. `S` → JSON → `S` reproduces every field of §2.2 bit-exactly except the derived fields (`amplitude`, `activecontacts`, `monopolarmodel`), which are recomputed; if the input's derived fields were inconsistent, the difference is logged.

**Import.** `dbsloc stim import <stimparameters.mat> [--label <l>]` and the GUI import read MAT v5/v6/v7 (`fastwarp-matfile::mat5`) and v7.3 (`fastwarp-matfile::matv73`), convert legacy files (§2.2), and write the canonical JSON with `provenance.imported_from` = the source path and its sha256. Existing Lead-DBS label folders found under `stimulations/native/` or `stimulations/<space>/` without our JSON are listed as importable; the stage never imports silently.

### 2.5 Model settings (`model_settings`) and the reference defaults

Values in the reference factory preferences (`common/ea_prefs_default.mat`, read 2026-10-23):

| Reference preference | Default | Our key | Meaning |
| --- | --- | --- | --- |
| `horn_ethresh` | 0.2 (V/mm) | `threshold_v_per_mm` (all models with a threshold) | Activation threshold. Settings in V/mm; files and solvers in V/m (×1000). |
| `fastfield_ethresh`, `dembek_ethresh` | 0.2 (V/mm) | same key | Same meaning per model. |
| `butenko_ethresh` | 200 (V/m) | — | OSS-DBS threshold, already in V/m (values < 1 are treated as V/mm by the reference with a warning). |
| `horn_cgm` / `horn_cwm` | 0.33 / 0.14 S/m | `conductivity.gm_s_per_m` / `wm_s_per_m` | Grey / white matter. GUI presets: "General heuristic (e.g. Buzsáki 2006)" 0.33 / 0.14; "Frequency adapted (10 kHz …)" 0.1365 / 0.09104 (annotated in the reference as adjusted for 1 kΩ at 10 kHz with a 0.1 mm grey-matter encapsulation, no double layer). |
| `horn_useatlas` = 1, `prefs.vat.gm` = `mask`, `horn_atlasset` = `DISTAL Minimal (Ewert 2017)` | — | `conductivity.mode = "gm_wm"`, `gm_mask = "atlas:<set>"` | Grey matter = the atlas set's `gm_mask.nii.gz` (`11` §2.6), warped to native space for native computations; everything else white matter. Other reference choices: the atlas's grey-matter structures, or the TPM grey-matter class > 0.5. |
| `horn_removeElectrode` | 1 | `remove_electrode` | Collapse the lead's volume out of the VTA (§3.8). |
| `fastfield_cb` | 0.1 S/m | — | FastField conductivity (not computed). |
| `dembek_pw`, `dembek_ethreshpw` | 60 µs, 60 µs | `dembek.reference_pulse_width_us` (60) | Dembek pulse-width scaling (§6.3). |
| `estimateInTemplate` | 0 | — | Not offered: we always compute in native space (D3). The reference forces template space for Kuncel, Mädler, Dembek and FastField; only SimBio and OSS-DBS compute in native space. |
| `butenko_encapsulation` | `None` | `encapsulation` | OSS-DBS only; its encapsulation thickness default in the integration scripts is 0.0 mm. The SimBio model has **no** encapsulation layer. |
| `prefs.vat.efieldmax` | 10 000 V/m | (fixed) | Clamp of written E-field values. |

Threshold presets offered in the reference GUI (V/mm), for our preset list: general heuristic 0.2; GPi 0.2; STN 0.19; VIM 0.165; and the Åström table by axon diameter D and pulse width: D 2.0 µm: 0.765 / 0.457 / 0.376 / 0.323 (30 / 60 / 90 / 120 µs); 2.5 µm: 0.504 / 0.323 / 0.240 / 0.310; 3.0 µm: 0.376 / 0.240 / 0.185 / 0.157; 3.5 µm: 0.300 / 0.185 / 0.142 / 0.121; 4.0 µm: 0.240 / 0.150 / 0.115 / 0.096; 4.5 µm: 0.225 / 0.142 / 0.107 / 0.090; 5.0 µm: 0.177 / 0.110 / 0.087 / 0.074. The entry 2.5 µm / 120 µs (0.310) breaks the monotone trend and may be a transcription error in the reference (**unverified** against Åström et al. 2015). Presets are copied into `threshold_v_per_mm`; the threshold is not derived from the source pulse width automatically.

Our further keys: `conductivity.mode` ∈ {`gm_wm` (default), `homogeneous`}; `homogeneous_s_per_m` (default 0.1 when `homogeneous`); `encapsulation.thickness_mm` (default 0 = off) and `encapsulation.s_per_m` (required when thickness > 0; no default, literature values 0.05–0.2 S/m are **unverified**); `source_combination` ∈ {`max` (default, reference behaviour, §3.5), `sum`}.

### 2.6 Validation rules

Checked when settings are saved (GUI, CLI, server API) and again at stage start; an error refuses the save / fails that stimulation with `vat: E_VAT_settings: <side> source <j>: <reason>`.

1. **Active source** = `amplitude > 0` and at least one contact with `percent > 0` and polarity ≠ `off`. Sources that are not active are ignored (no error), whatever their contacts say.
2. `amplitude` finite and ≥ 0. Warnings (not errors) above 10 V (voltage) or 20 mA (current), and for Mädler outside 0–10 V or impedance outside 500–2000 Ω (the paper's validity range).
3. `percent` ∈ [0, 100]; a contact with `percent > 0` must have polarity ≠ `off`, and a contact with polarity ≠ `off` must have `percent > 0` (the reference GUI keeps them consistent).
4. **Current mode:** per polarity present in the source, the sum of `percent` over contacts **plus the case when the case has that polarity** is 100 within 0.01 (the reference GUI redistributes percentages exactly this way: the case counts in its polarity group). Error otherwise.
5. **Voltage mode:** percentages only mark contacts as used (every used contact carries the full source voltage, §3.3). A sum ≠ 100 is a warning.
6. **Return path.** The source must have current return: either both polarities on the lead, or the case participating (`case.polarity ≠ off` and `case.percent > 0`). A source with only one polarity on the lead and the case off is accepted with `dbsloc: warning: vat: <label>: <side> source <j> has no return electrode; the case is used as return` and computed with the case participating (this matches the reference FEM, which grounds the outer boundary whenever no contact is an anode).
7. Contact indices must exist on the side's model: a contact index > N with `percent > 0` is an error; with `percent = 0` it is dropped with a note.
8. `impedance_kohm` > 0; missing or ≤ 0 → 1 kΩ with a warning (only `maedler` and `dembek` read it).
9. **Analytical models** (`kuncel`, `maedler`, `dembek`): exactly one used contact per active source (`E_VAT_montage`, the reference's one-contact rule; the reference raises it when more than one contact has `percent > 0`); the case is ignored. `kuncel` and `maedler` require voltage mode (`E_VAT_mode`; the reference GUI disables the mode switch and fixes voltage for them). `dembek` accepts both (§6.3).
10. `label`: 1–64 characters of `[A-Za-z0-9._-]`, not starting with `.`; `gs_` prefixes are reserved for group stimulations (`specs/14`). Imported folders with other names are kept as they are and flagged.
11. `threshold_v_per_mm` in (0, 10]; conductivities > 0; `encapsulation.thickness_mm` ∈ [0, 1].

Scope of fields: per source = `mode`, `amplitude`, `pulse_width_us`, `frequency_hz`, `case`, `contacts[]` (including `impedance_kohm`, which is per source in `S`; differing impedances of one contact across sources are a warning); per side = `active_source`, the electrode model (from the reconstruction, not from the settings); per stimulation = `label`, `model`, `model_settings`.

---

## 3. E-field computation contract (`model = efield`)

This section fixes **what** is computed. The discretisation (finite elements, finite volumes or finite differences on nested regular grids) is the implementer's choice, subject to the accuracy requirements of §9.1.

### 3.1 Geometry

Per side, in native space:

- **Lead frame.** The map `T` of `05` §6 built from the side's native markers (`head`, `tail`, `x`, `y` of the Reco `native` block; the `scrf` block instead when it exists, as the reference prefers brain-shift-corrected coordinates, **unverified** for every reference path) and the catalogue model: rotation, translation and the stretch `s` along the lead. The reference places its electrode mesh with the same four-point affine (verified). The contacts of the volume conductor are therefore exactly the Reco `coords_mm` contacts.
- **Electrode body** from the `09` catalogue: lead radius `r = lead_diameter / 2`; per contact its axial extent (centre ± `contact_length / 2` in the model frame, before the axial stretch) and, for segmented contacts, its angular extent: centre azimuth per `09` §10.2, arc **90°** (measured on the reference electrode meshes of Boston Vercise Directed, Medtronic B33005 and Abbott Directed 0.5: every segment spans 90.0°); ring contacts span 360°; a `tipiscontact` model's tip contact includes the rounded tip. Everything else of the lead body (shaft, tip cap, the gaps between segments, the orientation marker) is **insulation**. The shaft extends proximally to the domain boundary.
- Deviation from the reference: the reference meshes the orientation marker(s) of directed leads as contact material (they are listed among the model's contact surfaces, verified for Boston Vercise Directed and Medtronic B33005); we model the marker as insulation, as it is on the device.
- An optional **encapsulation layer**: a shell of thickness `encapsulation.thickness_mm` around the whole lead surface, conductivity `encapsulation.s_per_m`.

### 3.2 Conductivity model

Quasi-static, isotropic, piecewise-constant conductivity σ(x):

| Region | σ | Reference value |
| --- | --- | --- |
| Grey matter (voxels of the grey-matter mask > 0.5 after resampling to native space through the inverse normalization, nearest neighbour or linear ≥ 0.5) | `gm_s_per_m` | 0.33 S/m |
| White matter (everything else in the domain) | `wm_s_per_m` | 0.14 S/m |
| Homogeneous mode | `homogeneous_s_per_m` | — |
| Encapsulation shell (optional) | `encapsulation.s_per_m` | none in the reference FEM |
| Contacts | ideal conductor (equipotential surface) | reference: 10⁸ S/m |
| Insulation | perfect insulator (zero normal current) | reference: 10⁻¹⁶ S/m |

Implementations model contacts as equipotential constraints and insulation as zero-flux boundaries (or excluded nodes) rather than as extreme conductivities, so the system stays well conditioned in single precision. Grey matter unavailable (no atlas set, no mask, no inverse warp): fall back to homogeneous white matter with `dbsloc: warning: vat: no grey-matter mask (<why>); homogeneous σ = <wm> S/m`, recorded in the sidecar.

### 3.3 Governing equations and boundary conditions

Let Ω be the tissue domain (outside the lead body), Γ_k the surface of contact k, Γ_ins the insulation surface, Γ_∞ the outer boundary. Potential φ (V), current density J = −σ∇φ, field E = −∇φ.

- In Ω: ∇·(σ ∇φ) = 0.
- Γ_ins: σ ∂φ/∂n = 0.
- Every contact is equipotential: φ = V_k on Γ_k (V_k a constant). Its net current is I_k = ∫_{Γ_k} −σ ∂φ/∂n dS (positive = current leaving the contact into tissue, i.e. anodic).
- **Inactive contacts float:** V_k unknown, I_k = 0.
- **Current-controlled active contact:** I_k prescribed, V_k unknown.
- **Voltage-controlled active contact:** V_k prescribed.
- **Case participating** (`case.polarity ≠ off`, `case.percent > 0`, or forced by §2.6 rule 6): the case is the far-field return, modelled as φ = 0 on Γ_∞ (Dirichlet).
- **Case not participating:** σ ∂φ/∂n = 0 on Γ_∞ (Neumann), Σ_k I_k = 0, and the potential is fixed only up to a constant (pin one degree of freedom or use the constrained form of §3.4).

**Prescribed values per active source** (amplitude A, used contacts U = cathodes C ∪ anodes P):

- *Current mode:* I_k = −A·perc_k/100 for k ∈ C, I_k = +A·perc_k/100 for k ∈ P (A in mA → A/1000 in A). With the case participating, the case carries the remainder (its percent of its polarity), which the Dirichlet outer boundary absorbs automatically.
- *Voltage mode, case participating:* contacts of the case's polarity are at 0 V; contacts of the other polarity at −A (cathodes) or +A (anodes). Monopolar cathodic stimulation is therefore V = −A on every cathode with the outer boundary grounded (as in the reference).
- *Voltage mode, case not participating:* cathodes at −A/2, anodes at +A/2, outer boundary insulated (as in the reference's bipolar voltage case, verified: it halves the contact potentials and drops the grounded boundary when any contact is an anode).
- Contact impedances are **not** used (no interface model), as in the reference FEM.

Deviation from the reference FEM: the reference never reads `case`; it grounds the outer boundary exactly when no contact is an anode and otherwise treats the setting as bipolar with an insulated boundary, even when the case shares the anodic percentage. The rules above honour the case (D4). For Lead-DBS comparisons (§9.2) use settings where the two conventions coincide: monopolar with the case as the only anode, or bipolar with the case off.

### 3.4 Solving: per-contact basis and superposition (normative choice)

The problem is linear, so every setting of a lead is a combination of N **basis solutions**. For each contact k of the lead, solve the case-participating problem with I_k = 1 mA, every other contact floating, Γ_∞ grounded. Record its field E^(k)(x) (vector, V/m per mA) and the contact potentials Z_jk = V_j (kΩ = V/mA), the transfer-impedance matrix (symmetric by reciprocity).

- **Current mode:** E = Σ_{k∈U} I_k E^(k), I in mA. Exact for the case-participating problem. For bipolar current (Σ I_k = 0, case off) the same sum is used: the net current through Γ_∞ is zero and, with the boundary at the distance of §3.7, the dipole field there is negligible; the error against a strictly insulated boundary is part of the §9.1 tolerance.
- **Voltage mode, case participating:** solve Z_UU · I_U = V_U (|U| × |U|) for the contact currents, then sum as above. The floating contacts stay floating because every basis solution keeps them floating.
- **Voltage mode, case off:** solve the bordered system [Z_UU 1; 1ᵀ 0] · [I_U; c] = [V_U; 0] (c = common offset), then sum.

This reproduces a direct solve of §3.3 to solver precision (requirement A4, §9.1) and needs N solves per lead and conductivity model, cached (§3.6). An implementation may instead solve each source directly; results must agree within A4.

### 3.5 Multiple sources

Sources of one side are **independent pulse trains** (interleaved in time), so their fields are not superposed. Per side, compute the field of each active source s separately (§3.4: contacts **within** one source are superposed, they fire together). The combined magnitude is, per point, **|E| = max_s |E_s|** (reference behaviour of both the FEM path and the OSS-DBS path, verified). `source_combination = "sum"` (|Σ_s E_s|, for simultaneous sources) is available but not the default. The binary VTA is thresholded from the combined magnitude. Per-source volumes are recorded in the sidecar (§5.4); per-source images are not written to the Lead-DBS folder.

### 3.6 Basis cache and instant updates

The basis of a lead (fields E^(k) on the solver grid + Z) depends only on: side, native markers, catalogue model, conductivity model (mode, values, grey-matter mask file sha256 and the inverse warp used to place it), encapsulation, domain and grid parameters, engine version. It is cached under `<derivatives root>/sub-<id>/headmodel/native/` as `sub-<id>_hemi-<R|L>_desc-efieldbasis.<ext>` + `….json` (key fields and their sha256), a format chosen by the implementer (internal, never read by Lead-DBS; the reference's own files in that folder are `sub-<id>_desc-headmodel<side>.mat` / `…_desc-hmprotocol<side>.mat` and are left alone). A cache whose key differs is rebuilt. With a valid cache any setting is a weighted sum, which serves the programming-support use case (GUI amplitude drag, server preview) without a stage run (§7.8).

### 3.7 Domain, grid and resolution

- **Reference (for orientation):** a bounding cylinder around the lead in the model frame of radius 40·f mm, from 20·f mm below the tip to max(15 mm, contact span) + 20·f mm above it, f = 0.5 (radius 20 mm) unless the largest amplitude of the side exceeds 4 (then f = 0.3125·max amplitude / 2.5); linear tetrahedra (maximum element volume 3 mm³ in the mesher call, **unverified** as the effective size near contacts); preconditioned conjugate gradients (incomplete Cholesky, tolerance 1e-9, at most 5000 iterations); E constant per tetrahedron; the |E| of tetrahedra inside active contacts replaced by the mean of the top 0.1 % (all verified).
- **Ours (requirement, not method):** the outer boundary is at least 40 mm from every contact (a grounded boundary closer than that biases voltage-mode fields; the reference's 20 mm cylinder makes its voltage-mode fields slightly stronger, by roughly a/R ≈ 3 % for a = 0.635 mm, R = 20 mm). Resolution near the lead must resolve the lead radius (0.635–0.7 mm) and the segment gaps; nested regular grids (e.g. 0.1 mm in a 20 mm cube around the contacts inside a 0.5 mm grid of the full domain) are the recommended structure for wgpu (matrix-free stencil, CG with Jacobi or multigrid preconditioning, host-side f64 reductions as in `fastwarp-reg`). Convergence: iterate until the relative residual is ≤ 1e-6 and the §9.1 requirements hold; report iterations and residual in the sidecar. Non-convergence within the iteration cap fails the stimulation with `E_VAT_solver`.
- **Determinism:** two runs on the same backend and inputs give byte-identical output files.

### 3.8 From field to VTA (per side, per space)

1. |E| is sampled on the **output grid**: axis-aligned in the space's world coordinates, isotropic spacing 0.2 mm (`vat_grid_mm`), covering the bounding box of the supra-threshold region ± 5 mm, origin and extent rounded outward to whole voxels, at most 400 voxels per axis (larger: clipped with a warning). The reference uses 100 samples per axis over the same ±5 mm box (verified), so its spacing varies between subjects; no reference consumer we found requires 100³ (**unverified** for every consumer).
2. **Electrode removal** (`remove_electrode`, default true, reference default): the space occupied by the lead is collapsed. Let a₁ = head − 5 mm·û (the first point of the Reco trajectory, `12` §4) and ρ the distance from the lead axis. For points with axial coordinate (x − a₁)·û > 0, the value at radial distance ρ is taken from radial distance ρ + r at the same axial position and azimuth; points inside the lead (ρ < r before the shift) contribute nothing. Points distal of a₁ are unchanged. (Reference: it deletes the contact and insulation elements and moves every remaining sample in that half-space radially inward by r.) With `remove_electrode = false`, voxels inside the lead body have |E| = 0.
3. Clamp: |E| > 10 000 V/m → 10 000 V/m.
4. **Binary VTA** = voxels with |E| > E_thr (strict), E_thr = 1000 · `threshold_v_per_mm` V/m.
5. **Raw volume** V_raw = count × voxel volume (mm³). This is the value the reference writes to `S.volume(side)` and `vatvolume`.
6. **Final binary** = fill enclosed holes, then morphological opening (erosion then dilation) with a ball of radius 3 voxels (voxels within distance ≤ 3 voxel spacings), as the reference does before its final write of the binary file (verified). V_final = its voxel count × voxel volume.
7. `efieldgauss` (§5.3) from the clamped |E| of step 3 inside the step-4 mask.

### 3.9 Volume figures

`S.volume(side)` and the `.mat` `vatvolume` = V_raw of the space the file belongs to. `ea_stats` volume = V_final (the reference measures the binary file after opening; verified, the two differ). The sidecar records V_raw, V_final and per-source raw volumes for both spaces.

---

## 4. Native space and warping

1. **Computation space:** always native (D3), on the lead frame of §3.1 from the Reco `native` markers. A Reco file without a `native` block for a stimulated side fails the stimulation with `E_VAT_no_native`.
2. **Template outputs** are derived from the native field, never recomputed in template space: for every voxel centre q of the template output grid, p = W(q) with W the forward normalization applied as an image transform (`normalization/transformations/sub-<id>_from-anchorNative_to-<Space>_mode-image_xfm.h5`, reference grid = the template output grid, `01` §7a, `04` §11), and |E|_template(q) = trilinear sample of the native |E| (after electrode removal and clamp) at p; 0 outside the native grid. No Jacobian modulation (|E| is a field value, not a density). The template output grid follows §3.8 step 1 with the supra-threshold bounding box found by mapping the native supra-threshold voxel centres through the point warp of `05` §6 (`from-<Space>_to-anchorNative` composite as the point transform). Steps 4–7 of §3.8 then run on the template values. Analytical models: the native binary sphere is warped by the same image transform with linear interpolation and thresholded at ≥ 0.5.
   - Reference behaviour, for comparison: the FEM path maps each tetrahedron midpoint from native to template space point by point and re-interpolates the scattered values onto a new 100³ grid (verified). Both give the warped field; small differences are expected at the boundary.
3. **Reference-space copy.** When the template space is not `MNI152NLin2009bAsym` (the reference installation's space, which Lead Group expects), the template outputs are additionally resampled into `MNI152NLin2009bAsym` through the template bridge (`04` §5.4a; the image direction target → reference uses `InverseComposite.h5` of the bridge cache) and written to `stimulations/MNI152NLin2009bAsym/<label>/`. Bridge unavailable: `dbsloc: warning: vat: no reference-space copy (<why>)`, no failure.
4. **Both are stored** (native and template folders). Group analyses read the **template** (or reference-space) files (`specs/14`).
5. **Missing warp:** native outputs are written; template and reference outputs are skipped with `dbsloc: warning: vat: <label>: no normalization transform; template-space outputs skipped`; the result says `native only` (§7.4) and group analyses refuse the subject.
6. The `vatgrad` arrows (§5.2) in the template file are placed at the warped sample points and rotated by the linear part of the normalization (`…_desc-affine_mode-image_xfm.mat`, `01` §7a). (The reference re-uses the unrotated native vectors at the template points; not replicated.)

---

## 5. Outputs

### 5.1 Folders and names

Under `<derivatives root>/sub-<id>/stimulations/<space>/<label>/` for `<space>` ∈ {`native`, `<Space>`, `MNI152NLin2009bAsym` (only when it differs from `<Space>`)}; `<slug>` per §1.1; `<H>` ∈ {`R`, `L`}. Lead-DBS reads uncompressed **`.nii`** under exactly these names (its consumers match `…_hemi-R.nii` with an end anchor, verified for the sweet-spot loader), so these images are an exception to the `.nii.gz` rule of `01` §7a.

| File | Content | Written for |
| --- | --- | --- |
| `sub-<id>_desc-stimparameters.mat` | `S` (§2.2), MAT v7 Level-5 | every space folder |
| `sub-<id>_sim-binary_model-<slug>_hemi-<H>.nii` | final binary VTA (§3.8 step 6), int16 0/1 (reference datatype) | every model |
| `sub-<id>_sim-efield_model-<slug>_hemi-<H>.nii` | clamped \|E\| in V/m, float32 | `efield` only (the analytical models write none, as in the reference) |
| `sub-<id>_sim-efieldgauss_model-<slug>_hemi-<H>.nii` | §5.3, float32 | `efield` only |
| `sub-<id>_sim-binary_model-<slug>_hemi-<H>.mat` | `vatfv`, `vatvolume`, `vatgrad` (§5.2), MAT v7 | every model (`vatgrad` only for `efield`) |
| `sub-<id>_desc-stimulation.json` | canonical settings (§2.3) | `native` only |
| `sub-<id>_desc-vta.json` | results sidecar (§5.4) | every space folder |
| `sub-<id>_desc-vtastats.json` | atlas statistics (§8) | template (and reference) folder |

Only hemispheres with at least one active source get images; a side without stimulation writes no image (the reference likewise writes nothing for an empty side). Files of a side or model that the current run does not produce are removed at commit (staged write, `00` §5.2). NIfTI header: sform = qform = the output-grid affine, codes copied from the anchor image (native) or the template image (template), `descrip` = `dbsloc vta`. Not written: the reference's `4D_efield_model-ossdbs` and `_S<k>` per-source files, `hemidesc-Flipped…` mirrors (`specs/14`), `seed-dMRI` / `seed-fMRI` files (`specs/15`), the reference head model.

### 5.2 MAT contents (MAT v7 Level-5, `fastwarp-matfile::mat5`)

- `sub-<id>_desc-stimparameters.mat`: one variable `S` (§2.4 export), classes as in §2.2 (`double`, `char`, `cell`, `struct`), plus an optional second variable `dbsloc_provenance` (struct of char fields: version, git hash, source JSON sha256, written_at), following `12` §7. Lead-DBS loads `S` by name.
- `…_hemi-<H>.mat`: `vatfv` (1×1 struct, `faces` F×3 double 1-based, `vertices` V×3 double in the folder's world mm), `vatvolume` (double, V_raw mm³), and for `efield` `vatgrad` (1×1 struct, column vectors `x`, `y`, `z` = arrow positions and `qx`, `qy`, `qz` = arrow vectors: unit field direction scaled so the longest arrow is 0.2 mm, at about one in ten solver samples inside the domain; display only). The reference builds `vatfv` as an iso-surface of the binary at level 0.75 with caps, smoothed; ours may use the viewer's marching cubes and smoothing (`atlas_marching.rs`) on the final binary; the mesh is for display only.
- Lead-DBS writes the per-subject statistics `sub-<id>_desc-stats.mat` as v7.3; we write v7 (its loader accepts both), see §8.4.

### 5.3 `efieldgauss`

Inside the binary mask of §3.8 step 4 (other voxels 0): rank-based inverse-normal transform of the |E| values (Van Albada & Robinson 2007): sort the M values, give each its rank m = 1..M (ties broken by order, i.e. split), map to z = Φ⁻¹((m − 0.5)/M) with Φ the standard normal CDF; subtract the minimum z so the smallest becomes 0; divide by the sum so the map sums to 1 (verified). The reference stores this map in an int16 file with SPM scaling (inherited datatype); we store float32.

### 5.4 Results sidecar `sub-<id>_desc-vta.json`

Keys: `schema` (`dbsloc-vta`, version 1), `label`, `model`, `space`, `software` (`{product, version, git_hash, target_os, target_arch, written_at}`, as `05` §8 provenance), `run_options` (serialised `RunOptions` of the stage), `settings_sha256` (of the canonical JSON), `model_settings` (resolved, every default filled in), `threshold_v_per_m`, `grid` (`{spacing_mm, dims, affine_4x4}` per side), `engine` (`{backend: "wgpu"|"cpu"|"analytic", adapter, solver_grids, iterations, relative_residual, basis_cache: "hit"|"built", seconds}`), `sides` (`{right|left: {status: "computed"|"no_stimulation"|"failed", reason?, volume_raw_mm3, volume_final_mm3, sources: [{index, mode, amplitude, volume_raw_mm3, contact_currents_ma?, contact_potentials_v?}], conductivity_fallback?}}`), `inputs` (fingerprint list as `00` §6: Reco JSON, the normalization composites, the grey-matter mask, the bridge files when used), `warnings` (list), `lead_dbs` (`{model_name, slug}`).

---

## 6. Analytical models

Sphere models centred on the single used contact of each active source (Reco native `coords_mm` row of that contact; the reference uses the contact centre in the space it computes in). Radius r in mm; voltage U in V; impedance Z in Ω (= `impedance_kohm` × 1000 of that contact); thresholds E in V/m. The reference uses the source `amp` as U whatever the mode (verified); our mode rules are in §2.6 rule 9.

### 6.1 Kuncel 2008

r = √((U − U₀) / k), U₀ = 0.1 V, k = 0.22 V/mm². U ≤ U₀ → empty VTA with a warning (the reference returns 0 only for U = 0 and fails for 0 < U < U₀). **Acceptance:** U = 3 V → r = 3.6307 mm; 1 V → 2.0226 mm; 2 V → 2.9388 mm.

### 6.2 Mädler 2012 (eq. 3)

r = −(k₄Z − √(k₄²Z² + 2k₁k₄Z + k₁² + 4k₃U) + k₁) / (2k₃), with k₁ = −1.0473, k₃ = 0.2786, k₄ = 0.0009856 (constants of the paper; verified in the reference). Validity per the paper: 500–2000 Ω, up to 10 V, monopolar. **Acceptance:** 3 V, 1000 Ω → 3.3941 mm; 2 V, 1000 Ω → 2.7923 mm; 3 V, 500 Ω → 4.4242 mm; 3 V, 2000 Ω → 2.0185 mm.

### 6.3 Dembek 2017

r = (pw / pw_ref)^0.3 · √(0.72 · I / E_thr) · 1000, with I = U/Z in A (voltage mode) or the source current in A (current mode), E_thr = 1000 · `threshold_v_per_mm` V/m, r from metres to mm. pw = the source `pulse_width_us`, pw_ref = `dembek.reference_pulse_width_us` (60). **Acceptance:** 3 V, 1 kΩ, 0.2 V/mm, 60 µs → 3.2863 mm; 90 µs → 3.7114 mm; current mode 3 mA, 0.2 V/mm, 60 µs → 3.2863 mm.
Deviations (decisions O4/O5): the reference takes pw from its machine preference `dembek_pw`, not from the stimulation, and treats a current-mode amplitude as volts.

### 6.4 Voxelisation and outputs

Per side, the union over active sources of the spheres. Grid (reference rule, verified): per axis, lower bound = floor(min over spheres of centre − r), upper bound = ceil(max of centre + r) in whole mm, n = 5 × (upper − lower) voxel centres spaced evenly from the lower to the upper bound inclusive (spacing (upper − lower)/(n − 1), slightly above 0.2 mm). A voxel is inside when its centre lies within r of a sphere centre (exact test; the reference tests against the convex hull of a randomised 100-point sphere sample, which is slightly smaller and not reproducible). Output = binary int16 NIfTI + the `.mat` of §5.2 + `S`; no E-field files; `vatvolume` = Σ over sources (4/3)πr³ as the reference returns it, and the sidecar also records the voxel-count volume. No hole filling or opening. Native → template per §4 step 2. **Acceptance:** two runs give identical bytes; the voxel-count volume of one 3 V Kuncel sphere is within 2 % of (4/3)π·3.6307³ = 200.47 mm³.

---

## 7. Stage contract (`vat`)

### 7.1 One implementation

`dbsloc_pipeline::vat` owns settings I/O (§2), the engine (§3, §6), warping (§4), writing (§5) and statistics (§8). The pipeline stage, the GUI (stimulation editor and live preview), `dbsloc run`, `dbsloc stim …` and the server call it; no front end computes a VTA itself. On the server the `vat` stage runs inside the per-job `dbsloc_pipeline_runner` like every other stage (`docs/SERVER_CLIENT_PLAN.md`); settings reach it through an API that writes the canonical JSON (endpoint design belongs to the server plan).

### 7.2 What a run does

The stage computes every stimulation label of the subject that has a canonical JSON in `stimulations/native/` (or the labels in `RunOptions.vat_labels` when set), excluding `gs_*` labels unless the run comes from a group job (`specs/14`). A label is **up to date** when its native `sub-<id>_desc-vta.json` records the same `settings_sha256`, `model_settings`, engine version and an `inputs` fingerprint that matches the files now (same comparison as `00` §6); up-to-date labels are kept (logged `dbsloc: note: vat: <label> unchanged; kept`), others are computed. `overwrite_approved` recomputes all.

### 7.3 Prerequisites (message prefix `vat:`)

- Reco JSON present with a `native` block for every stimulated side (`stage prerequisite missing: vat: no reconstruction (run reconstruct)`).
- Electrode model of each stimulated side resolvable in the catalogue and suitable (§10).
- For template outputs: the forward normalization composite (missing → native only, §4 step 5; not a prerequisite failure).
- For `gm_wm`: the atlas set's `gm_mask.nii.gz` and the inverse composite (missing → homogeneous fallback, §3.2).

### 7.4 Results and messages

- Completed: `ok:vat:<computed>/<labels>` (e.g. `ok:vat:2/3`, one label kept), with ` (native only)` appended when any label lacked the warp.
- Skipped: `skip:vat: no stimulation settings` when the subject has no label; `skip:approved` per `00` §5.2; `plan:vat` with `plan_only`.
- Failed (`stage_failed` message): `vat: E_VAT_settings: …`, `vat: E_VAT_montage: …`, `vat: E_VAT_mode: …`, `vat: E_VAT_model: …`, `vat: E_VAT_elmodel: …`, `vat: E_VAT_no_native: …`, `vat: E_VAT_solver: …`, `vat: E_VAT_capacity: …`, `cancelled by user`. One failing label fails the stage after the other labels have been attempted (their outputs commit); the failure lists every failed label.
- This replaces the current `skip:vat not implemented (specs/07)` token of `00` §5.2 (follow-up edit of `00`).

### 7.5 Lock and fingerprint

`locks/vat.json` as `00` §6. Fingerprint inputs for `vat`: the Reco JSON, every canonical stimulation JSON computed, the forward and inverse normalization composites, the grey-matter mask; this replaces the current `vat` input list of `00` §6 ("the first normalized post-op", follow-up edit). An approved, fingerprint-matching lock skips the stage (`skip:approved`). The stage writes its lock unapproved (`approval: false`) after a run; approving is a user action.

### 7.6 Invalidation

`vat` → `visualize` (`00` §6.1, unchanged). `reconstruct`, `orientation` and a manual correction invalidate `vat` (already in the graph); a changed stimulation JSON makes its label not up to date (§7.2) without touching other labels. Group results that read a subject's VTA files record that label's `settings_sha256` and output sha256 and are stale when they change (`specs/14`).

### 7.7 Batch, cancellation, resources, logging

- **Batch:** in `run_batch` (`00` §5.2) `vat` runs per subject like any stage; a failing subject does not stop the batch unless `on_error = stop`.
- **Cancellation:** the engine polls the run's cancel flag between labels, between basis solves and at every solver iteration; a cancel commits nothing for the running label (staged writes, `00` §5.2) and fails the stage with `cancelled by user`.
- **Backend:** `RunOptions.vat_backend` ∈ {`auto` (default), `wgpu`, `cpu`} (env `DBSLOC_VAT_BACKEND`, same precedence as `00` §5.2). `auto` uses the wgpu device of the registration engine (same adapter selection, self-test and one-device-per-process rule, `04` §5.4) when a capacity pre-flight fits the solver grids in the GPU budget (same budget rule as `04` §5.4: `DBSLOC_GPU_MEMORY_BUDGET_MB`, else reported device memory, halved on integrated or software adapters, minus 1 GiB), else the CPU engine with `dbsloc: note: vat: <why>; using the CPU engine`. Explicit `wgpu` that does not fit fails with `E_VAT_capacity`. The CPU engine is the f64 reference implementation and must meet the same §9.1 requirements. Analytical models run on the CPU. CPU workers respect `ants_max_threads`.
- **Logging:** one line per label: `dbsloc: vat: <label>: <model>, right <V_raw> mm³, left <V_raw> mm³, <backend>, <seconds> s`; warnings as specified above; the sidecar keeps the details.

### 7.8 Interactive use (programming support)

The GUI and the server may request a **preview** for unsaved settings: the same engine with the cached basis (§3.6), native space only, no files, returns |E| and the binary on the output grid plus volumes. Saving the settings and running the stage produces the files. Target latency in §9.3.

---

## 8. Atlas overlap statistics

Computed per label, per side, in **template space** (and in the reference space for the reference-space copy), for the atlas set chosen for statistics (`RunOptions.vat_stats_atlas`, default the default atlas set of `11` §1, DISTAL Minimal). Native-space statistics are not computed (the reference computes native-folder statistics against template atlas files, which we do not replicate).

### 8.1 Atlas mask per structure and side

Per `11` §2: structure files by type (1 right `rh/`, 2 left `lh/`, 3 both `rh/` + `lh/` chosen by the side, 4 `mixed/`, 5 `midline/`); fibre, surface and `discfibers` entries are skipped and get intersection 0, normalised 0 and atlas volume NaN (reference behaviour). For a right VTA, atlas voxels with world x < 0 are zeroed; for a left VTA, x > 0 (RAS, applied to every type, verified). Then the mask = voxels > 0.5 × the maximum of the split image. Atlas volume = mask voxel count × atlas voxel volume (on the atlas grid).

### 8.2 Overlap on the VTA grid

The binarised atlas mask (0/1) is resampled onto the VTA grid with trilinear interpolation and thresholded at > 0.5. With B = the final binary VTA (> 0.5 × its maximum, i.e. the 0/1 file):

- `AtlasIntersection` = |B ∩ A| × VTA voxel volume (mm³);
- `nAtlasIntersection` = |B ∩ A| / |B| (fraction of the VTA; 0 when |B| = 0);
- `nWithinAtlasIntersection` = intersection mm³ / atlas volume (fraction of the structure);
- `AtlasVolume` = atlas volume (mm³);
- VTA volume = |B| × voxel volume (= V_final).

Reference behaviour for the record: it computes the fraction of the structure on the atlas grid (VTA resampled onto the atlas grid) and re-thresholds the resampled binary atlas at half of the **original** atlas maximum, which is wrong for atlases whose maximum is not 1 (verified); we threshold the resampled 0/1 mask at 0.5 and use the VTA-grid intersection for both fractions. Differences are a partial-voxel effect (provisional tolerance in §9.2).

### 8.3 E-field overlap (`efield` only)

E′ = the template E-field with values ≤ E_thr set to 0. `efield.volume` = Σ_voxels E′ (V/m summed over voxels, grid-dependent, reference definition); `efield.AtlasIntersection` = Σ E′ over voxels in the resampled atlas mask; `efield.nAtlasIntersection` = ratio (0 when the sum is 0). The reference thresholds with its FEM threshold preference whatever the model; we use the label's threshold. The sidecar also records the grid-independent integral Σ E′ × voxel volume (V/m·mm³).

### 8.4 Records

- **Ours:** `sub-<id>_desc-vtastats.json` in the template (and reference) folder: atlas set (name, citation, structure names and types in index order), per side the five quantities per structure, the E-field quantities, amplitude vector, model, settings sha256.
- **Lead-DBS `ea_stats`:** in `<derivatives root>/sub-<id>/sub-<id>_desc-stats.mat`, variable `ea_stats`, entry `stimulation(t)` with `t` = the index of the entry whose `label` equals the stimulation label (reference: existing entry reused; else appended): `label`; `vat(side, 1)` with `amp` (= 1×4 amplitudes of the side), `label`, `contact` (= 1), `side`, `volume` (= V_final), `AtlasVolume`, `AtlasIntersection`, `nAtlasIntersection`, `nWithinAtlasIntersection` (1×n_atlas each, atlas index order); `efield(side, 1)` with `volume`, `AtlasIntersection`, `nAtlasIntersection`. A side without stimulation gets zeros and NaN atlas volumes (reference behaviour when the amplitude is zero). The reference writes one `vat(side, s)` row per source for the analytical models, with identical overlap values; we write one row (decision O7). `ea_stats.atlases.names` / `types` must match the statistics' atlas set: when the file is absent we create it with `atlases` and `stimulation`; when it exists, is fully readable (`fastwarp-matfile`, v7 or v7.3) and its `atlases.names` equal ours, we update the one `stimulation` entry and rewrite the file as v7 keeping every other field; otherwise we leave it untouched and log `dbsloc: note: vat: sub-<id>_desc-stats.mat not updated (<why>); see sub-<id>_desc-vtastats.json`. Contact-to-atlas distances (`conmat*`) are `specs/14`.

---

## 9. Validation and acceptance criteria

### 9.1 Engine requirements (synthetic, CI)

Grid-based engines are tested with the electrode replaced by the stated geometry.

| # | Case | Requirement |
| --- | --- | --- |
| A1 | Sphere electrode radius a = 0.635 mm at potential V₀ = −3 V, concentric grounded sphere R = 50 mm, σ = 0.1 S/m: \|E(ρ)\| = \|V₀\| / (ρ² (1/a − 1/R)) | \|E\| within 5 % for ρ ∈ [1.5, 10] mm; threshold radius at 200 V/m within max(0.1 mm, one output voxel) of the analytic 3.106 mm (ρ_thr = √(\|V₀\| / (E_thr (1/a − 1/R)))) |
| A2 | Same sphere, current 3 mA: \|E\| = I / (4πσρ²) | within 5 %; ρ_thr at 200 V/m = 3.455 mm within max(0.1 mm, one voxel) |
| A3 | A2 with an encapsulation shell (0.2 mm, 0.05 S/m) in voltage mode: I = V₀ / R_s with R_s = (1/4π)[(1/σ_e)(1/a − 1/(a+d)) + (1/σ)(1/(a+d) − 1/R)], tissue field I/(4πσρ²) | within 5 % for ρ ≥ a + d + 0.5 mm |
| A4 | Superposition: basis sum (§3.4) vs direct solve of the same setting (one monopolar, one bipolar, one three-contact directional current setting) | max \|ΔE\| / max \|E\| ≤ 1e-4 (f32 engine), ≤ 1e-8 (CPU f64) |
| A5 | Floating contact (Medtronic 3389, contact 2 active, others off) | potential uniform on each floating contact within 1e-4 of the active potential; net current ≤ 1e-4 of the active current |
| A6 | Charge balance in current mode | flux through Γ_∞ equals the prescribed net current within 1 % |
| A7 | Grid convergence, 3389 contact 2, 3 V monopolar, homogeneous 0.1 S/m | V_raw changes ≤ 2 % when the fine grid spacing is halved |
| A8 | Rotational symmetry, directed lead, one segment at 2 mA, then the next segment of the same level | Dice ≥ 0.95 between the second VTA and the first rotated by 120° about the lead axis |
| A9 | Mirror symmetry: the same setting on a lead mirrored through x = 0 | Dice ≥ 0.98 after mirroring |
| A10 | Determinism | two runs byte-identical per backend; CPU vs wgpu Dice ≥ 0.99, V_raw within 1 % |
| A11 | Settings | JSON ↔ `S` round trip of §2.4 on fixtures: default `S`, bilateral multi-source multipolar `S`, legacy (`k0`–`k15`) `S`, v7.3 `S` |
| A12 | Analytical | the §6 acceptance values within 1e-4 mm; byte-identical re-runs |

### 9.2 Comparison with the reference (Phase D, provisional tolerances)

On 5 Lead-Tutor subjects (no stimulation settings ship with the cohort; use fixed settings): (a) 3389-class lead or directed lead contact 2, monopolar 3 V; (b) 3 mA current mode monopolar; (c) one directional segment at 2 mA; (d) bipolar 2–3, 3 V, case off; (e) two sources (contact 2 at 2 mA, contact 5 at 2 mA). Reference outputs come from Lead-DBS 3.2.x SimBio (native computation, `horn_useatlas` default, threshold 0.2 V/mm) produced on a MATLAB installation; comparing outputs is allowed (`AGENTS.md` §9). Metrics per side in template space:

- Dice of the final binary VTAs (our grid resampled onto theirs, nearest): ≥ 0.85 for (a), (b); ≥ 0.75 for (c)–(e).
- Volume ratio V_final(ours) / V_final(reference) ∈ [0.8, 1.25].
- Pearson r of log₁₀ \|E\| over voxels where either field exceeds 50 V/m: ≥ 0.9.
- Statistics: \|Δ nAtlasIntersection\| ≤ 0.05 for STN and GPi of DISTAL Minimal.

These tolerances are **provisional** (FastField reports a mean Dice of 0.92 against FEM, Baniasadi et al. 2020); they are fixed in Phase D after the first comparison. OSS-DBS may be run separately as a second reference.

### 9.3 Performance targets (provisional)

| Operation | GPU (integrated, e.g. Intel Arc 140V) | CPU fallback |
| --- | --- | --- |
| Basis of one lead (8 contacts, gm_wm) | ≤ 60 s | ≤ 10 min |
| One stimulation, both sides, from a cached basis, including warp, files and statistics | ≤ 5 s | ≤ 30 s |
| Preview update (§7.8), one side, native | ≤ 0.3 s | ≤ 2 s |
| Analytical model, one stimulation | < 1 s | < 1 s |

GPU memory: the solver grids for one lead must fit in 1.5 GiB (the budget left on the reference integrated adapter after the viewers).

---

## 10. Edge cases

| Case | Behaviour |
| --- | --- |
| No active contact in any source of a side, or all amplitudes 0 | Side status `no_stimulation`, no images for it; stats zeros / NaN atlas volume; not an error. Both sides: label result `empty`, still counted as computed. |
| Settings for a side without a reconstructed lead (unilateral Reco) | Active sources on that side: warning, side skipped (`no_lead`). |
| Contact index beyond the model (e.g. 8-contact directed settings on a 4-contact 3389) | `percent > 0`: `E_VAT_settings: contact <k> does not exist on <model>`; `percent = 0`: dropped with a note. |
| Directional settings on a ring model, or ring-only settings on a directed model | Accepted as long as indices exist (the catalogue order defines what each index is). |
| Electrode model unknown, single-level, or not for stimulation (`forstimulation = 0`) | `E_VAT_elmodel`. Every model with catalogue contact geometry is supported, SEEG models included. |
| Model `fastfield` / `ossdbs` requested | `E_VAT_model: <model> outputs can be imported but not computed; use efield`. |
| Analytical model with 0 or > 1 used contacts in an active source | `E_VAT_montage`. |
| Kuncel / Mädler in current mode | `E_VAT_mode`. |
| Missing or non-positive impedance | 1 kΩ, warning. |
| No return electrode | §2.6 rule 6 (case used, warning). |
| Missing normalization warp | Native only (§4 step 5). |
| Missing grey-matter mask or inverse warp | Homogeneous white matter, warning (§3.2). |
| Reco without `native` block | `E_VAT_no_native`. |
| Template space ≠ `MNI152NLin2009bAsym` and no bridge | No reference-space copy, warning. |
| Supra-threshold region touching the domain boundary | The domain is enlarged once automatically (basis rebuilt); if the region still touches it, warning `dbsloc: warning: vat: <label>: <side>: the VTA reaches the model boundary` and the result is kept. |
| Output grid larger than 400 voxels per axis | Clipped, warning. |
| Solver does not converge | `E_VAT_solver` for that label. |
| GPU too small / device lost | `auto`: CPU engine (note); explicit `wgpu`: `E_VAT_capacity`; a lost device is recreated once (`04` §5.4) before failing. |
| Two leads with different models | Computed per side; `S` export padded (§2.4). |
| Legacy `S` (no `ver`) | Converted on import (§2.2). |
| `S.amplitude` disagrees with `amp` | `amp` used, warning (§2.4). |
| Threshold given in V/m in a settings field that expects V/mm (value > 10) | `E_VAT_settings` (rule 11). |
| Existing Lead-DBS outputs in the label folder from another model | Removed at commit only if they carry our slug; files of other slugs are left untouched (they are Lead-DBS's). |

---

## 11. References

- Horn A, Li N, Dembek TA, et al. (2019) Lead-DBS v2: Towards a comprehensive pipeline for deep brain stimulation imaging. *NeuroImage* 184:293–316. doi:10.1016/j.neuroimage.2018.08.068
- Horn A, Reich M, Vorwerk J, et al. (2017) Connectivity predicts deep brain stimulation outcome in Parkinson disease. *Ann Neurol* 82(1):67–78. doi:10.1002/ana.24974 (the "Horn 2017" FEM VTA model)
- Vorwerk J, Oostenveld R, Piastra MC, Magyari L, Wolters CH (2018) The FieldTrip-SimBio pipeline for EEG forward solutions. *Biomed Eng Online* 17(1):37. doi:10.1186/s12938-018-0463-y
- Kuncel AM, Cooper SE, Grill WM (2008) A method to estimate the spatial extent of activation in thalamic deep brain stimulation. *Clin Neurophysiol* 119(9):2148–2158. doi:10.1016/j.clinph.2008.02.025
- Mädler B, Coenen VA (2012) Explaining clinical effects of deep brain stimulation through simplified target-specific modeling of the volume of activated tissue. *AJNR Am J Neuroradiol* 33(6):1072–1080. doi:10.3174/ajnr.A2906
- Dembek TA, Barbe MT, Åström M, et al. (2017) Probabilistic mapping of deep brain stimulation effects in essential tremor. *NeuroImage Clin* 13:164–173. doi:10.1016/j.nicl.2016.11.019
- Baniasadi M, Proverbio D, Gonçalves J, Hertel F, Husch A (2020) FastField: An open-source toolbox for efficient approximation of deep brain stimulation electric fields. *NeuroImage* 223:117330. doi:10.1016/j.neuroimage.2020.117330
- Butenko K, Bahls C, Schröder M, Köhling R, van Rienen U (2020) OSS-DBS: Open-source simulation platform for deep brain stimulation with a comprehensive automated modeling. *PLoS Comput Biol* 16(7):e1008023. doi:10.1371/journal.pcbi.1008023
- Åström M, Diczfalusy E, Martens H, Wårdell K (2015) Relationship between neural activation and electric field distribution during deep brain stimulation. *IEEE Trans Biomed Eng* 62(2):664–672. doi:10.1109/TBME.2014.2363494 (threshold table; the reference labels it "Astrom 2014", the online-first year)
- Butson CR, Cooper SE, Henderson JM, McIntyre CC (2007) Patient-specific analysis of the volume of tissue activated during deep brain stimulation. *NeuroImage* 34(2):661–670. doi:10.1016/j.neuroimage.2006.09.034
- Van Albada SJ, Robinson PA (2007) Transformation of arbitrary distributions to the normal distribution with application to EEG test–retest reliability. *J Neurosci Methods* 161(2):205–211 (`efieldgauss`; volume and pages **unverified**).
- User guide: https://netstim.gitbook.io/leaddbs

### Notes for the orchestrator (open decisions taken in this spec)

- O1 `efield` results are written under the reference's SimBio display name and slug `simbio` (only six names are accepted by the reference); our engine identity lives in the sidecar.
- O2 Canonical settings file `sub-<id>_desc-stimulation.json` (native folder), not `…_desc-stimparameters.json`, which the reference's programmer integration already uses for a different content.
- O3 The case participates in the field (§3.3), unlike the reference FEM.
- O4 Dembek uses the source pulse width, O5 Dembek current mode uses the current directly.
- O6 Analytical spheres are computed in native space and warped (the reference computes them in template space).
- O7 One `ea_stats` `vat(side, 1)` row per side.
- O8 Multi-source combination = per-voxel maximum (reference), `sum` optional.
- O9 Marker modelled as insulation.
- Follow-up edits outside this file: `00` §5.2 (`vat` tokens and messages) and §6 (`vat` fingerprint inputs), `01` §7a (stimulation names, `.nii` exception, `headmodel/` cache), `docs/RUN_REFERENCE.md` (`vat_backend`, `vat_labels`, `vat_stats_atlas`, `DBSLOC_VAT_BACKEND`, `dbsloc stim list|show|set|import`), `docs/SERVER_CLIENT_PLAN.md` (`vat` stage, settings endpoint). Made 2026-10-23 (documentation only, implementation pending); cross-references with `14` and `15` aligned the same day (`14` §2.3–§2.5, §3.2–§3.4, §4.1–§4.5, §8, §9; `15` §5.1, §7.1, §9.2, §11). No change to this spec was needed.

### Implementation notes (Phase C, 2026-10-23; normative where this spec left a choice open)

- **§2 CLI syntax.** `dbsloc stim list [--json]`, `dbsloc stim show <label> [--json]`, `dbsloc stim set <label> --from <json>` (normalises, validates, writes the canonical JSON and the native `…_desc-stimparameters.mat`; refuses a file whose `label` differs), `dbsloc stim import <stimparameters.mat> [--label <l>]`; all take `--output-dir` and `--no-index`; exit 0 ok, 1 validation / I/O failure, 2 BIDS error. `frequency_hz` is JSON-only. `provenance.imported_from` = `{path, sha256}`.
- **§3.4 / §3.7 solver choices.** One rectilinear grid in the lead frame with geometrically graded cells (fine spacing 0.25 mm, growth 1.25, outer boundary 40 mm from every contact), cell-centred finite volumes with harmonic-mean face conductances; contact cells share one unknown (exact equipotentials), so the 1e8 / 1e-16 S/m constants are not used; Jacobi-preconditioned CG in f64 on the CPU, relative residual 1e-6, cap 5000 iterations. The wgpu engine runs the same system in f32 with f64 host reductions and stops at max(1e-6, 1e-5); it reports the true f64 residual. The domain is enlarged once by 1.5 when the VTA touches the boundary. Directional segments span 90° with an inner radius of half the lead radius; the marker is insulation.
- **§3.6 basis cache.** `headmodel/` keeps the per-contact basis as f64 `.bin` + `.json` (key: geometry, conductivity, grid, engine version); contacts are solved lazily and merged; the record names the backend that built it, and either backend's basis is accepted.
- **§3.8 step 2.** The removal collapse at ρ = 0 uses azimuth 0; the supra-threshold bounding box is taken before the collapse.
- **§5.2.** `vatfv` is the voxel-boundary surface of the final binary with Taubin smoothing (no GPU dependency); `vatgrad` is thinned to at most 1500 supra-threshold cells and omitted from the reference-space copy; analytical models write the sphere sum as `vatvolume` in every space.
- **§5.4.** Per-source volumes are recorded in the native sidecar only (`null` elsewhere). `settings_sha256` hashes the canonical JSON with `provenance` blanked, so a no-op re-save keeps the results.
- **§7.5.** The lock fingerprint lists the Reco JSON, every computed stimulation JSON and the forward / inverse composites; the grey-matter mask is recorded in the sidecar `inputs` instead.
- **§7.7.** `RunOptions.vat_labels` (`DBSLOC_VAT_LABELS`, comma separated) and `vat_stats_atlas` (`DBSLOC_VAT_STATS_ATLAS`) exist beside `vat_backend` (`DBSLOC_VAT_BACKEND`); CLI flags `--vat-backend`, `--vat-labels`, `--vat-stats-atlas`. An explicit `wgpu` fails only for the `efield` model; analytical models always run on the CPU.
- **§8.** A structure without a file for the VTA's side gets intersection 0 and a NaN atlas volume; atlas entries without a numeric type get NaN in `atlases.types`. Statistics are computed in the template folder when the template space is the atlas space, else in the reference-space folder. An existing `sub-<id>_desc-stats.mat` is updated in place only when it is MAT v7; a v7.3 file is left untouched with a logged note (deviation from §8.4 until a v7.3 writer exists).
- **§9.1.** Measured: CPU vs wgpu `Z` within 1.2e-4 and |E| within 3e-5 relative on the spec domain (73 × 73 × 109 cells); wgpu 0.32 s per contact vs 1.84 s CPU (release, Intel Arc 140V).

*End spec 07.*
