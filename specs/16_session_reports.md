# 16 — Vendor session reports: import of programming data from manufacturer PDFs

**Status:** implemented, partial (`docs/STATUS.md` row 14 lists the open items); specified 2026-10-23. Owner decisions in §11. Sources: this module reads vendor documents, not Lead-DBS; the only reference material is the vendor PDFs themselves and their vendor JSON exports used as test ground truth. No code from third-party extractors is reused.

## 1. Purpose and scope

Clinicians receive the programming state of an implanted pulse generator (IPG) as a **session report** PDF printed by the manufacturer's programmer. The report is often the only machine-readable record of the stimulation settings a patient was discharged with, so it is the natural input for VTA modelling (`specs/07`) and group analyses (`specs/14`).

This spec defines:

1. a **vendor-neutral record** (`dbsloc-session-report`, §3) that holds everything we extract from a report — identity, device, leads, stimulation groups per snapshot, history, impedances, events, sensing summaries — with per-field provenance and diagnostics;
2. a **generic PDF reading layer** and a **vendor module contract** (§2, §6) so that further vendors (Boston Scientific, Abbott) plug in without touching the shared code;
3. the **Medtronic Percept PC / RC session report** module (§7), the first and only implementation at this version;
4. **anonymisation** (§4): name and identifiers dropped, every date shifted by one patient-constant random offset;
5. **storage in the subject tree** and the **mapping to stimulation settings** (§5);
6. CLI, GUI and server surfaces (§8), tests (§9), edge cases (§10).

Out of scope: OCR of scanned reports (the reports carry a text layer; a scan is refused with `E_REPORT_scanned`), vendor JSON exports as input (the Medtronic `Report_Json_Session_Report_*.json` is used only as test ground truth; reading it as input is a later addition under the same record), sensing time series (LFP trend logs) beyond the printed summaries.

## 2. Architecture

| Layer | Crate / module | Contents |
| --- | --- | --- |
| PDF text and geometry | `crates/dbsloc-reports/src/pdf/` | One function `read_document(path) -> PdfDocument`: pages with `Glyph { text, x0, top, x1, bottom, baseline, size, font, fill_rgb, alpha, render_mode }` and `Shape { kind: Rect \| Curve \| Line \| Image, x0, top, x1, bottom, stroke, fill, line_width: Option<f64>, stroke_rgb, fill_rgb, alpha, subpaths, curves }` in **points, origin top-left** (pdfplumber convention, so that the numbers in §7 can be checked with any tool; glyph box = origin to advance in x, `Descent` to `Descent + 1 em` in y). Colour and alpha are part of the record because the Medtronic report draws its off-contact markers as white glyphs and its hidden tokens at alpha 0.004 (§7). Decoding through `lopdf` (D3) and our own interpreter: graphics state `q Q cm w gs` (ExtGState `/LW /CA /ca /Font`), colours `RG rg G g K k SC SCN sc scn`, text `BT ET Tf Tc Tw Tz TL Ts Tr Tm Td TD T* ' " Tj TJ`, fonts simple / Type3 (`FontMatrix`) / Type0 CIDFontType2 Identity-H (`/W`, `/DW`), Unicode from `/ToUnicode` first (lopdf's own lookup prefers `/Encoding`, which is garbage for Type3 fonts with synthetic glyph names), else the simple-font encoding, else U+FFFD (counted); paths `m l c v y h re` painted by `S s f F f* B B* b b* n`, line width × √\|det CTM\|, Form XObjects recursed with `/Matrix`, image XObjects as boxes; clipping, dashes and inline images ignored. Never panics on content. Helpers: `lines(bounds)` (glyphs grouped by baseline within 2 pt, words split at gaps > max(1.5 pt, 0.2 × size)), `text_in(bounds)`, `find(label)`. No rendering, no images. |
| Record | `crates/dbsloc-reports/src/model.rs` | The §3 types (serde), `Diagnostic`, `FieldSource`. |
| Vendor modules | `crates/dbsloc-reports/src/vendors/<vendor>/` | `impl VendorModule` (§6). `medtronic_percept` at this version. |
| Registry | `crates/dbsloc-reports/src/lib.rs` | `parse_report(path, &ParseOptions) -> Result<SessionReport, ReportError>`: reads the document once, asks every module `detect(&doc)`; exactly one match parses; none = `E_REPORT_unknown_vendor` (error messages never contain text from the document; the first page's first lines are available only from `report inspect` (not with `--anonymize-preview`) and the import dialog; they are stripped of control characters and capped at 120 characters per line); several = `E_REPORT_ambiguous`. |
| Anonymisation | `crates/dbsloc-reports/src/anonymize.rs` | §4, pure functions over the record. |
| Subject import | `crates/dbsloc-pipeline/src/reports.rs` | §5: writes the record into the subject tree, archives the PDF, derives stimulation labels through `vat::settings`. |
| CLI / GUI / server | `dbsloc report …`, GUI *Import session report…*, server endpoints | §8. |

`dbsloc-reports` depends on the PDF crate, `serde`, `serde_json`, `jiff` (dates) and `fastwarp-core` (SHA-256); it never depends on `dbsloc-pipeline`.

## 3. The record `dbsloc-session-report` (version 1)

JSON, UTF-8, keys in the order below. Every value that could not be read is `null`, never 0 or an empty string. Dates are ISO 8601; a **local** date-time without zone is written as `YYYY-MM-DDTHH:MM` (the report prints local time without a zone), a date-only value as `YYYY-MM-DD`, an instant as RFC 3339 with `Z`.

```json
{
  "schema": "dbsloc-session-report",
  "schema_version": 1,
  "vendor": "medtronic",
  "report_kind": "percept_session_report",
  "source": { "filename": "Report_SessionReport_20260930T090539.pdf", "sha256": "…", "pages": 10, "printed_at": "2026-09-30T09:05", "parser": { "product": "dbsloc", "version": "…", "git_hash": "…" }, "parsed_at": "2026-10-23T12:00:00Z" },
  "anonymized": false,
  "patient": { "name": "Demo Patient", "patient_id": "MRN:123456", "sex": "male", "date_of_birth": "1959-06-25", "diagnosis": "Parkinson's disease", "clinician_notes": "Demo Session Notes" },
  "device": { "model": "Percept PC", "model_number": "B35200", "serial": "NPI_FOLLOWUP_PERCEPT_PC", "implanted": "2026-03-04", "location": "left chest", "rechargeable": false, "battery_percent": 75, "battery_estimate": "4 years, 1 month", "firmware": { "product_version": "07.00.00", "core_version": "00.00.00", "device_version": "00.00.00", "product_application_id": "0000", "core_application_id": "0000", "device_application_id": "0000" }, "feature_code": "160d", "programmer_version": "5.0.676" },
  "session": { "date": "2026-09-30T08:51", "timezone": null, "stimulation_initial": "on", "stimulation_final": "on", "battery_reminder": "11:00", "close_communication_required": true },
  "leads": [ { "hemisphere": "left", "vendor_model": "3389", "catalogue_model": "Medtronic 3389", "target": "STN", "contacts": [ { "label": "0", "device_number": 0, "k": 1 }, { "label": "1", "device_number": 1, "k": 2 }, { "label": "2", "device_number": 2, "k": 3 }, { "label": "3", "device_number": 3, "k": 4 } ], "orientation_deg": null, "tip_offset": null }, { "hemisphere": "right", "vendor_model": "3389", "catalogue_model": "Medtronic 3389", "target": "STN", "contacts": [ { "label": "8", "device_number": 8, "k": 1 }, "…" ] } ],
  "snapshots": {
    "initial": { "active_group": "A", "groups": [ "…" ] },
    "final":   { "active_group": "A", "groups": [ {
      "id": "A", "name": "Walking", "active": true,
      "hemispheres": { "left": { "programs": [ {
          "index": 1,
          "mode": "current", "amplitude": 5.7, "unit": "mA", "pulse_width_us": 60, "frequency_hz": 130,
          "limits": { "lower": null, "upper": null },
          "case": "anode",
          "contacts": [ { "label": "0", "k": 1, "polarity": "cathode", "amplitude": 1.0 }, { "label": "1", "k": 2, "polarity": "cathode", "amplitude": 1.9 }, { "label": "2", "k": 3, "polarity": "cathode", "amplitude": 0.8 }, { "label": "3", "k": 4, "polarity": "anode", "amplitude": null } ],
          "contact_sum_matches_amplitude": false,
          "sensing": null
        } ] }, "right": { "programs": [ "…" ] } },
      "cycling": null, "soft_start_stop": null
    } ] }
  },
  "history": [ { "date": "2026-09-01T10:12", "groups": [ "… same shape as a snapshot group …" ] } ],
  "group_usage": [ { "id": "A", "percent": 47.0 } ],
  "impedances": [ { "hemisphere": "left", "kind": "monopolar", "a": "case", "b": "0", "ohm": 1022, "status": "ok" }, { "hemisphere": "left", "kind": "bipolar", "a": "0", "b": "1a", "ohm": 2682, "status": "ok" } ],
  "events": [ { "date": "2026-09-12T18:30", "name": "Tremor", "count": 3, "hemisphere": null } ],
  "annotations": [ { "date": "2026-03-30", "hemisphere": "left", "frequency_hz": 130, "mode": "current", "program": "…", "symptoms": [ { "name": "rigidity", "score": 2 } ] } ],
  "sensing": { "surveys": [], "streaming": [], "timeline": [] },
  "fields": [ { "path": "snapshots.final.groups[0].hemispheres.left.programs[0].amplitude", "status": "read", "page": 3, "bbox": [0, 0, 0, 0], "rule": "mdt.program.amplitude" } ],
  "diagnostics": [ { "severity": "warning", "code": "W_REPORT_contact_sum", "path": "snapshots.final.groups[0].hemispheres.left.programs[0]", "message": "printed contact currents sum to 3.7 mA, program amplitude is 5.7 mA" } ],
  "sections": { "pi": "Demo Patient\nDiagnosis: …", "di": "…" }
}
```

Rules:

- `vendor` ∈ {`medtronic`, `boston_scientific`, `abbott`} (closed list, extended with new modules); `report_kind` is the module's document type id.
- **Contacts.** Every contact carries the vendor's printed `label` (`"0"`, `"1a"`, `"8"`, …), the vendor `device_number` when the report prints one (Medtronic ports: left 0–7, right 8–15), and `k`, the 1-based Lead-DBS contact index of the catalogue model (`specs/09`, so `k` = 1 is the most distal contact; for an eight-contact directional lead `k` 2–4 and 5–7 are the segments of the two levels in the catalogue's A/B/C order). The mapping label → `k` is the vendor module's responsibility and is stated per model in §7.
- **Polarity** ∈ {`off`, `cathode`, `anode`}; `case` ∈ {`off`, `cathode`, `anode`}.
- **Per-contact amplitude** is the printed value only (`null` when the report prints none, e.g. for the anode in a monopolar setting). The record never redistributes amplitudes; `contact_sum_matches_amplitude` is `true` when the printed cathode currents sum to the program amplitude within 0.05 mA, `false` when they do not, `null` when fewer than two per-contact values are printed.
- **Programs.** One entry per printed program of a hemisphere in a group (two for interleaving). `index` is 1-based in print order.
- `fields` holds one entry per extracted scalar with `status` ∈ {`read`, `derived`, `missing`, `ambiguous`, `not_reported`} and the PDF location; `diagnostics` holds every warning or error of the parse (codes `W_REPORT_*` / `E_REPORT_*`, listed in §10).
- `sections` keeps the plain text of every section the module recognised (key = the module's section id) **only when the record is not anonymised**.
- Unknown vendor fields the module reads but the schema has no key for go into `extra` objects at the same level (`patient.extra`, `device.extra`, `program.extra`, …), never silently dropped.

## 4. Anonymisation

`anonymize(record, &Shift) -> SessionReport`, applied before anything is written when the import runs with `--anonymize` (CLI), the *Anonymise* tick (GUI) or `anonymize: true` (server). The function:

1. removes `patient.name`, `patient.patient_id`, `patient.clinician_notes`, `device.serial` and `device.extra`, and every `sections` text; `patient.extra` is dropped entirely;
2. keeps `patient.sex`, `patient.diagnosis`, the device model and firmware, every stimulation value, impedance, event name and count;
3. **shifts every date and date-time in the record by the same offset** `Shift { days: i32 }` with `−30 ≤ days ≤ 30`: `patient.date_of_birth`, `device.implanted`, `session.date`, `source.printed_at`, every `history[].date`, `events[].date`, `annotations[].date`, every date inside `sensing` and `extra`. Times of day are unchanged, so intervals, seasons and ages are preserved (age at session is exact because birth and session move together);
4. sets `anonymized: true` and `anonymization: { "method": "date-shift", "range_days": 30 }` — **the offset itself is never written**;
5. `source.filename` becomes `<vendor>_<report_kind>_<shifted session yyyyMMddHHmm>.pdf` (the real file name may carry a patient name) and `source.sha256` is kept (it identifies the input without revealing it).

**Offset per patient.** The offset must be the same for every report of one patient, else intervals between reports break. It is derived, not stored: `days = (HMAC-SHA256(key, "dbsloc-date-shift:" + patient_key) mod 61) − 30`, where `key` is a 32-byte secret in `<user data dir>/anonymization.key` (created on first use as the SHA-256 of two OS-seeded `RandomState` hashes, the time and the process id, file mode 0600 on Unix; `--key-file` or `DBSLOC_ANONYMIZATION_KEY_FILE` overrides the path) and `patient_key` is the dataset subject id (`sub-<id>`) the report is imported into. Consequences, stated to the user: the same subject id gives the same shift as long as the same key is used; a lost key changes the shift of future imports (the import warns `W_REPORT_shift_mismatch` when the subject already has an anonymised report whose shifted date of birth differs from the one the current key would produce — a check that needs no stored offset because the true date of birth is in the PDF being imported). The key never enters the dataset.

**What anonymisation cannot do.** Free-text fields of the vendor (group names like "Walking", event names) are kept; a clinician may have typed a name into them. The import prints `W_REPORT_free_text` listing the kept free-text fields once per report so the user can review them. The source PDF is **not** archived in an anonymised import (§5).

## 5. Storage and stimulation settings

### 5.1 Files

| Path (under `derivatives/leaddbs/sub-<id>/`) | Content |
| --- | --- |
| `clinical/reports/<vendor>/<session yyyyMMddHHmm>_<sha256[:8]>/sub-<id>_desc-sessionreport.json` | The §3 record (anonymised or not). `<session …>` is the (shifted) local session date-time. |
| `clinical/reports/<vendor>/<…>/sub-<id>_desc-sessionreport.txt` | The page text (one page per `\f`), non-anonymised imports only; for debugging a parse. |
| `clinical/reports/index.json` | `{ "reports": [ { "folder", "vendor", "session", "anonymized", "sha256", "labels": [] } ] }`, rewritten on every import; `folder` is relative to `clinical/reports/` (e.g. `medtronic/202609300851_eb850562`), `session` is the record's (shifted) session date-time, `labels` are the labels derived by the latest import of that report plus earlier ones that still exist on disk; the GUI and `dbsloc report list` read it. |
| `stimulations/native/<label>/sub-<id>_desc-stimulation.json` (+ `.mat` twin) | One label per imported group (§5.2), written through `vat::settings::save` after `normalise` + `check`. |

The source PDF is copied to `sourcedata/sub-<id>/reports/<vendor>/<original filename>` (`.partial-*` staging, `[-N]` on a name clash with different content, skipped when the same sha256 is already there) unless the import is anonymised or `--no-source-copy` is given. Re-importing the same sha256 into the same subject replaces the record folder (same folder name) and re-derives its labels; it is not an error.

### 5.2 Stimulation labels

Options: `snapshot` ∈ {`final` (default), `initial`, `both`, `none`}, `groups` (default: every group that has at least one program with amplitude > 0 in that snapshot), `label_prefix` (default none).

Label = `[<prefix>_]<session yyyyMMdd>_<group id>[_initial]`, e.g. `20260930_A`, `20260930_C_initial`; validated by `label_is_valid`; a label that exists with a **different** settings sha256 gets `_2`, `_3`, … (`unique_label` rule); an identical one is kept and reported as unchanged.

Mapping group → `Stimulation` (`specs/07` §2.3):

- `subject` = the id; `model` = `efield`; `model_settings` = defaults; `label` as above.
- Side mapping: hemisphere `right` → `sides.right`, `left` → `sides.left` (the report's hemisphere wording, never the port number).
- Program `j` of a hemisphere → `sources[j−1]` (`index` = j, 1 ≤ j ≤ 4; a program with another index is skipped with a warning); unused sources stay blank; `active_source` = 1.
- `mode` from the program (`current` / `voltage`); `amplitude` = the program amplitude; `pulse_width_us`; `frequency_hz`.
- Contacts: `contacts[k−1].polarity` = the contact's polarity; `percent`: **current mode** — when every cathode of the program has a printed amplitude, `percent_i = 100 × a_i / Σ a_cathodes` (rounded to 0.01, last one adjusted so the sum is exactly 100); otherwise equal shares among the cathodes; anodes on the lead likewise among themselves; the case gets 100 of its polarity when it is the only electrode of that polarity, else the share rule of `07` §2.6 rule 4 cannot be satisfied from the print and the import uses equal shares among the lead anodes with the case at 0 and records `W_REPORT_case_share`. **Voltage mode** — the used contacts of each polarity take equal shares that sum to 100 within that polarity (the percentages only mark usage, `07` §2.6 rule 5). `impedance_kohm`: the session's monopolar impedance of that contact (Ω / 1000) when the report prints one, else 1 with no warning.
- The program amplitude wins over the printed contact sum (`07` §2.4: `amp` is what the models read); a mismatch is already a record diagnostic and is repeated as `dbsloc: warning: report: <label>: printed contact currents sum to 3.7 mA, amplitude 5.7 mA; percentages follow the printed shares`.
- `provenance.imported_from` = `{ "report": "<record path relative to the subject folder>", "sha256": "<report sha256>", "snapshot": "final", "group": "A" }`.
- Electrode model: the lead's `catalogue_model` must resolve in `specs/09`; its contact count sets N. When the subject's `uiprefs.mat` / reconstruction names a different model, the import warns `W_REPORT_model_mismatch` and still writes the label (the stage checks N at run time, `07` §2.6 rule 7). A lead whose vendor model is unknown to the module makes the label fail with `E_REPORT_lead_model` (the record is still written).
- A group whose programs are all at 0 amplitude yields no label unless `groups` names it explicitly (then a label with inactive sources, accepted by `check`).

## 6. Vendor module contract

```rust
pub trait VendorModule: Sync {
    fn vendor(&self) -> Vendor;
    fn report_kind(&self) -> &'static str;
    fn detect(&self, doc: &PdfDocument) -> bool;
    fn parse(&self, doc: &PdfDocument, opts: &ParseOptions) -> Result<SessionReport, ReportError>;
}
```

- `detect` reads only the first page (header strings, vendor marks) and must not panic on any input; it returns `true` only for a document type the module fully supports.
- `parse` fills the record; a missing optional section is a `fields` entry with `status: "not_reported"`, not an error; an unparsable mandatory section (identity, device, lead configuration, the final snapshot) is `E_REPORT_section:<id>` and aborts; everything else degrades to diagnostics.
- Every number the module writes into the record carries a `fields` entry with page, bbox and rule id. Rule ids are `<vendor>.<section>.<field>`.
- Modules are pure (no I/O, no clock, no randomness); the registry stamps `source` and `parsed_at`.
- Vendor-specific contact label → `k` tables live in the module and are unit-tested against `specs/09` contact counts.
- Adding a vendor = one folder under `vendors/`, one registry line, one synthetic fixture set under `tests/fixtures/reports/synthetic/` (the repository holds no manufacturer file; tests against the manufacturer's own reports are gated, §9) and one §7-style section in this spec.

## 7. Medtronic Percept PC / RC session report (`medtronic` / `percept_session_report`)

The measured layout is `specs/16a_medtronic_percept_layout.md` (normative for every coordinate, string and tolerance; its §3.9 / §3.10 tables are the acceptance list of §9). This section fixes the module's contract on top of it.

### 7.1 Detection

`detect` = page 1 contains the glyph line `Session Report` at the top left (12 pt, alpha-0 text over an outline, so the alpha-0 glyphs must be kept), a header line 3 starting with `Percept PC ` or `Percept RC ` (other Medtronic stimulators are refused until a report is available), the token `<s-pi>` and the footer `Page 1`. Locale: the English labels of 16a §2; any missing mandatory label refuses with `E_REPORT_section:<id>`.

### 7.2 Pages and sections

Pages are classified by their `<s-xx>` / `<e-xx>` tokens (16a §1.6): `pi` patient, `di` device, `ss` stimulation settings (header, lead configuration, groups A–D), `pps` patient programmer settings, `gs` group settings, `sii` impedance (repeated on a continuation page), `gh` group history, `gu` group usage, `rc` battery recharge (RC only), `sa` session annotations, `bss` / `mbss` BrainSense survey and electrode identifier, `pe` patient events, `es` event summary (`<e-s-es>` end token). Marker y positions are not section boundaries; state (current group column, column date, hemisphere) is carried across pages (16a §5.9). Mandatory: `pi`, `di`, the `ss` header + lead configuration, and the Final column of groups A–D.

### 7.3 Text rules

- Header line 2 = `<name> <patient id>` (16a §1.5); the name is kept whole (no first / last split).
- White glyphs (`.` contact markers, `--` empty-slot marks, section tokens) are removed from text lines before any value is parsed; alpha-0 black glyphs are kept (16a §5.11).
- `µ` U+00B5 and `μ` U+03BC are both accepted; NBSP = space; en dash in ranges; thousands separators in impedances; `Limit:3.3 mA` without space (16a §5.12–5.13).
- Dates per 16a §6: local wall-clock values, `MMM d, yyyy [h:mm a]`; `session.timezone` stays `null` (the report prints no zone); event-summary and recharge dates take their year from the session date (most recent date ≤ session).

### 7.4 Leads

From the lead-configuration boxes (16a §2.4): hemisphere, target, `Contacts: s–e` (s = 0 left or 8 right port range), `Lead Model: 3389 | 3387 | 3391 | B33005 | B33015`, tip offset and orientation when printed. `catalogue_model` = `Medtronic <model>`. Contact labels and `k`:

| Model | Labels (distal → proximal) | `k` |
| --- | --- | --- |
| 3389 / 3387 / 3391 | `s`, `s+1`, `s+2`, `s+3` | 1, 2, 3, 4 |
| B33005 / B33015 | `s`, `s+1a`, `s+1b`, `s+1c`, `s+2a`, `s+2b`, `s+2c`, `s+3` | 1, 2, 3, 4, 5, 6, 7, 8 |

The segment letters a / b / c map to the catalogue's segments A / B / C in that order (`specs/09` §7.2: A = +Y, counter-clockwise seen from proximal); the printed lead `orientation_deg` is recorded in the lead and not applied (the reconstruction's own orientation stage decides the roll).

### 7.5 Groups and programs

Per 16a §3: a group box per snapshot column (Initial x0 = 60, Final x0 = 315) and hemisphere; **active group = box border width 2.0** (0.3 inactive), which needs the ExtGState `/LW` tracking of the PDF layer; one shaft per program (two = interleaving, `W_REPORT_interleaving`); circles grouped into four rows (levels s+3 … s top → bottom), a directional level row holding three circles (a, b, c by x) or one circle with the eye icon in a BrainSense diagram (whole level); exactly one glyph or eye image per circle: black `-` 8.7 pt = cathode, black `+` 6.7 pt = anode, white `.` = off, eye = sensing (recorded in `program.sensing.contacts`, polarity `off`); any other row count or an empty circle is `E_REPORT_section:ss` for that group (the group is skipped, the rest of the report continues). Case = `anode` when no contact is `+`, else `off` (16a §3.4 rule 7; the print cannot show a mixed return). Amplitude / pulse width / rate lines and the per-contact current lines at the offsets of 16a §3.6; a single cathode gets its program amplitude as `amplitude` with `fields.status = derived`; anodes stay `null`. Group D (BrainSense): the printed amplitude is the suspend amplitude; `program.sensing = { "mode": "adaptive" | "sensing_only", "status": "adapting" | "paused" | null, "contacts": ["1","3"], "lfp_hz": …, "limits": {…}, "extra": {…} }` with the labelled values of 16a §2.5.

### 7.6 History, usage, impedances, annotations, events

- Group history (16a §2.9): one entry per session date column, groups A–D, slots 0–1 left and 2–3 right hemisphere, mapped with the session's **Final** lead configuration (`W_REPORT_history_lead` once per report); orphaned date headings are carried to the next page.
- Group usage (16a §2.10): the report prints no per-group percentage, only monthly totals under a bar chart, so `group_usage` lists the group ids with `percent: null` (`not_reported`) and the printed month totals go to `session.extra.group_usage_monthly` (`{month_start, total_percent}`); the bars are not read.
- Impedances (16a §2.8): monopolar `case`–contact and bipolar contact pairs in Ω, status from the printed marker; `TestCurrentMA` text kept in `extra`.
- Annotations (16a §2.12): date, hemisphere, amplitude / pulse width / rate, cathodes from the 6 pt diagram, symptom scores (`-` = not rated → `null`, `0` = rated none).
- Patient events (`session.extra.patient_events.{initial,final}`), event summary (16a §2.14–2.15; counts as `events[]` with `date: null` and `extra.{kind, period_start, period_end}`, the LFP summary in `sensing.extra.lfp_amplitude_summary`), battery recharge (RC, `device.extra.recharge`, coupling matched from the bar alpha to the legend swatch), BrainSense survey / identifier (`sensing.surveys` as `{kind, hemisphere, electrodes, peak_magnitude_uvp, peak_frequency_hz}`, `sensing.extra.identifier`; "No Data Available" → empty). Patient-programmer limits go to `program.limits` and `group.extra.patient_programmer.<side>`, group-settings sensing values to `group.extra.sensing_settings.<side>`, group D adaptive values to `program.sensing.extra`. `lead.tip_offset` = the first contact number of the port range (0 or 8, status `derived`); a differing Initial lead configuration goes to `lead.extra.initial`. History groups carry `active: false` with one `not_reported` field and a null name.

### 7.7 Unverified paths

Neither the manufacturer's reports nor the synthetic ones have a B33015 lead, a unilateral lead, cycling, a group with stimulation off, or a non-English locale (16a §4); the synthetic ring report has a voltage-mode program, the synthetic directional report an Initial `STIMULATION Off` header. The module parses them by the same rules and marks the result `fields.status = ambiguous` where the layout was extrapolated. The synthetic reports are written from `specs/16a`, so they prove that parser and layout reference agree, not how the manufacturer's software behaves; a manufacturer report is required before these become acceptance tests.

## 8. Surfaces

### 8.1 CLI (`dbsloc report …`)

- `dbsloc report inspect <pdf> [--json] [--anonymize-preview]` — parse without writing; prints vendor, session, device, leads, every group of the final snapshot as a table (hemisphere, program, amplitude, pulse width, rate, contacts with polarity and current), then the diagnostics. `--json` prints the record. `--anonymize-preview` prints what the anonymised record would contain with a zero shift (no key needed).
- `dbsloc report import <dataset_root> <subj_id> <pdf>… [--anonymize] [--snapshot final|initial|both|none] [--groups A,B] [--label-prefix P] [--no-source-copy] [--output-dir DIR] [--no-index] [--json]` — §5. Several PDFs are imported in order; one failure does not stop the others; exit 1 when any PDF or any derived label failed (a failed label is printed as `dbsloc: error: report: <label>: <reason>`; the record is still written), 2 on a BIDS error. The summary lists per PDF: record folder, labels written / unchanged, warnings.
- `dbsloc report list <dataset_root> <subj_id> [--json]` — the index.
- `dbsloc report anonymize <pdf> --subject <sub-id> --out <json>` — parse + anonymise to a file outside any dataset (for sharing a single report), same key rule.

Messages follow `00` §9: `dbsloc: error: report: E_REPORT_<code>: <text>`, `dbsloc: warning: report: …`.

### 8.2 GUI

- Top bar *Import…* menu gains *Session report (PDF)…*; the Pipeline panel's *Volume of activated tissue* card gains *Import session report…*. Both open a file dialog (several PDFs), then one dialog: detected vendor and session per file (parsed on a worker, with a spinner), *Anonymise* (off by default; explanation line under it: "Removes name, patient id, notes and serial number; shifts every date by a fixed random offset of at most 30 days."), *Create stimulation settings from* (Final / Initial / Both / None), the group ticks of the chosen snapshot (prefilled with the groups that have at least one program with an amplitude above 0), *Keep a copy of the PDF in sourcedata* (on; disabled and off when anonymising). *Import* runs on the worker, prints the §8.1 summary lines to the Output panel and refreshes the stimulation list; an open stimulation editor (not dirty, no run active) is re-opened so its label list shows the new labels (selecting the first new label is a later addition). The subject is resolved once when the dialog opens (one directory scan on the paint thread); parsing and importing run on one worker, a cancel takes effect between files. Keys while the window is open: Esc = cancel / stop after the current file / close, Enter = Import when enabled and no text field has focus.
- A *Reports* line on the *Volume of activated tissue* card lists the imported reports (session date, vendor, anonymised flag, labels) with *Show…* opening the record in a read-only text window.
- Screenshot mode: `DBSLOC_APP_SCREENSHOT_ACTION=report` with `DBSLOC_APP_REPORT_PDF=<path>` opens the dialog on that file.

### 8.3 Server (`docs/SERVER_CLIENT_PLAN.md`)

`POST /api/subjects/<id>/reports` (multipart PDF + options JSON) runs the import in the request thread (parse < 2 s per report), returns the summary; `GET /api/subjects/<id>/reports` returns the index; the record is served through the file allowlist as `clinical/reports/**`, the `.txt` only when the config enables `serve_report_text`. Anonymisation options as in §8.1. (Deferred until the client needs it; the endpoints are listed here so the names are fixed.)

## 9. Tests

- Fixtures `tests/fixtures/reports/synthetic/` (the repository holds no file produced by manufacturer software and no data extracted from one): `ring_percept_pc.pdf` (Percept PC, 3389 ring leads, 19 pages) and `directional_percept_rc.pdf` (Percept RC, B33005 directional leads, 21 pages), each with `<name>.expected.json`. `scripts/make_synthetic_session_report.py` (Python standard library only) writes all four files, deterministically (fixed dates and identifiers, no timestamps, so two runs give identical bytes); `uv run python scripts/make_synthetic_session_report.py` regenerates them, `tests/test_synthetic_session_report.py` regenerates into a temporary folder and compares bytes with the committed files, and checks that no vendor artwork, font, producer or metadata entered. Everything in the reports is invented: patient ("Sample Patient" / "Example Person", identifiers `SYN:0001` / `SYN:0002`), serials, dates, settings, impedances, events. Only the section titles, field labels, hidden `<s-xx>` / `<e-xx>` tokens and the positions that the parser keys on (`specs/16a`) mirror the manufacturer layout, because interoperability needs them. The text uses the standard Helvetica fonts (not embedded) with WinAnsi encoding and a small `ToUnicode` map (U+00A0, U+00B0, U+00B5, U+2013); the two images are invented 8 x 5 px (sensing icon) and 6 x 4 px (impedance status icon) bitmaps; page content is in native PDF coordinates with line widths set by `w` and fill alpha by `ExtGState` `ca`.
  - Both reports hold the header and footer, patient and device pages, the stimulation header and lead configuration (Initial and Final), groups A to D of both snapshots (one active group each, a two-program interleaved group, a bipolar group with anodes, per-contact currents that do not sum to the amplitude, BrainSense group D with sensing icons, adaptive limits and thresholds, group D right hemisphere on a continuation page without group heading, a voltage-mode program in the ring report, an unnamed group), patient programmer settings, group settings (soft start / stop, cycling, BrainSense sensing values), impedance tables (ring: both hemispheres on one page; directional: one page per hemisphere with 8 monopolar and 28 bipolar cells per lead, one `>10,000` cell), group history (four sessions in two columns over two pages, the dates of the last two printed at the bottom of the first page and their boxes on the next), group usage, session annotations (rotated column headers, scores `-`, `0` to `3`), patient events, event summary with the LFP summary of group D, and the hidden section tokens including the repeated `<s-sii>` start token of a continuation page and the `<e-s-es>` end token.
  - The ring report adds the PC battery estimate, a BrainSense survey (three plots) and an electrode identifier; the directional report adds the RC `ERI Date`, the battery recharge page (four bars, three coupling alphas, the metrics block), "No Data Available" survey and identifier pages, orientation lines and a printed `Off` reminder / initial stimulation state.
  - `<name>.expected.json` records what the generator printed, in a vendor-neutral shape (patient, session, device, leads, both snapshots with programs, contact polarities, printed currents, sensing contacts, adaptive values, history, impedances, annotations, usage, survey, identifier, events, summary, recharge, and the counts of contact-sum, interleaving and voltage diagnostics). The values are the generator's inputs, not parser output.
- Gated tests with the manufacturer's own reports: tests that compare the parser with the vendor JSON exports (`fixtures_match_vendor_json_and_structure`) and with the glyph, path and curve dumps (`first_fixture_matches_reference_text_and_layout`, `all_fixtures_parse_without_unmapped_glyphs`) read a folder named by the environment variable `DBSLOC_MEDTRONIC_FIXTURES`. The folder holds the four PDFs, `pairing.json`, `vendor_json/*.json.gz` and `expected/*.tsv`; none of it is in the repository (the manufacturer's material may not be redistributed). When the variable is set but does not name a folder, each test fails (a mistyped path must not look like a pass). When the variable is unset each test prints one line to standard error (`skipped <test>: <reason>`) and returns, so public CI runs the synthetic tests only and the gated tests run on a machine that holds the reports. What only the gated tests cover: the Skia page preamble (`1 0 0 -1 0 792 cm`, page clip), Type3 and Type0 (CID) fonts with vendor `ToUnicode` maps and one `Tj` per glyph with overlapping advances, line widths given through `ExtGState` `/LW`, alpha-0 text over vector-outline headings, the white alpha 0.0039 `--` empty-slot mark, the U+03BC survey unit, BrainSense "Sensing Only" boxes, and the agreement of all 72 stimulation programs, 180 history programs, 168 annotations and 184 impedance cells with the vendor JSON.

- `dbsloc-reports` unit tests: the PDF layer on the synthetic ring report (known glyph, circle, shaft, image and line-width positions, `µ` and en dash decoded, non-breaking spaces of bold headings, rotated column headers, fill alpha), the whole parse of both synthetic reports against their `expected.json` (patient, session, device, leads, every group of both snapshots with programs, polarities, printed and derived currents, sensing contacts, adaptive values, programmer limits, group settings; every history group; all impedance cells; all annotations; usage, survey, identifier, events, summary, recharge; the diagnostics counts; no `missing` field; sections kept only on request; JSON round trip; dropping any page never panics), label → `k` tables, anonymisation (every date moves by the same offset, nothing identifying remains: the anonymised JSON contains none of the strings of `patient.name`, `patient_id`, `serial`; age at session unchanged; a shift of +30 across a leap day), HMAC determinism and range.
- Pipeline tests: import into a synthetic subject writes the record, the index, the labels (`check` passes, percentages sum to 100 per polarity, case rules), the sourcedata copy; anonymised import writes no copy and no `.txt`; re-import is idempotent; unknown-vendor PDF and a PDF without text layer fail with the stated codes; `W_REPORT_model_mismatch` when `uiprefs.mat` says another model.
- CLI tests (`cli_report`): inspect / import / list round trip on the synthetic ring report; exit codes.
- GUI: `cargo test -p dbsloc-app report` (dialog state, group ticks, anonymise disables the copy tick).

## 10. Diagnostics and edge cases

| Code | Severity | Meaning / handling |
| --- | --- | --- |
| `E_REPORT_unknown_vendor` | error | No module detected the document. The message never contains text from the document; the first lines of page 1 are shown only by `report inspect` (stderr; not with `--anonymize-preview`) and the import dialog, never by `import`, `anonymize`, JSON output or logs. |
| `E_REPORT_ambiguous` | error | More than one module claimed it. |
| `E_REPORT_scanned` | error | No text layer (fewer than 20 glyphs on the first page). |
| `E_REPORT_section:<id>` | error | A mandatory section is missing or unreadable. |
| `E_REPORT_lead_model` | error (label only) | Vendor lead model unknown to the module or absent from `specs/09`. |
| `E_REPORT_label` | error (label only) | Label invalid after prefixing, or no free `_N` suffix. |
| `E_REPORT_settings` | error (label only) | `vat::settings::check` refused the derived settings (message appended). |
| `E_REPORT_key` | error | Anonymisation key file cannot be found, created or read. |
| `E_REPORT_io` / `E_REPORT_pdf` | error | File system or PDF decoding failure. |
| `W_REPORT_contact_sum` | warning | Printed contact currents do not sum to the amplitude. |
| `W_REPORT_case_share` | warning | Case share cannot be derived (§5.2). |
| `W_REPORT_model_mismatch` | warning | Report lead model ≠ the subject's electrode model. |
| `W_REPORT_shift_mismatch` | warning | Anonymisation key differs from the one used for this subject before. |
| `W_REPORT_free_text` | warning | Kept free-text fields listed (anonymised import). |
| `W_REPORT_unmapped_glyphs` | warning | Glyphs without Unicode mapping were seen (count, pages). |
| `W_REPORT_history_lead` | warning | Historical groups mapped with the session's lead configuration (the report prints no historical lead data). |
| `W_REPORT_interleaving` | note | Two programs on one hemisphere imported as sources 1 and 2. |
| `W_REPORT_section:<id>` | warning | An optional section (`gh`, `sii`, `sa`, `es`, …) could not be read; its fields are `not_reported`. |
| `W_REPORT_lead_model` | warning | Lead model unknown at record level (the label-level error is `E_REPORT_lead_model`). |
| `W_REPORT_active_group` | warning | More than one group box drawn with the active border. |
| `W_REPORT_contact_current` | warning | A printed current names a contact that is not on the lead. |

Further rules: a group whose hemisphere prints "Off" or no program yields no source; a program with amplitude 0 is kept in the record and skipped for labels; a lead configuration with one hemisphere only gives one-sided labels (`07` accepts an empty side); the Percept RC (rechargeable) prints recharge statistics that go to `device.extra`; a report of a different language locale is refused by `detect` (English labels only at this version; the vendor module notes the labels it keys on so that a locale table can be added).

## 11. Owner decisions (2026-10-23)

- **D1** Reports are first-class subject data under `clinical/reports/`; stimulation labels are derived, never the only copy of the data.
- **D2** Anonymisation = date shift with a patient-constant offset derived from a user-held key (§4); the offset is never stored.
- **D3** PDF crate: `lopdf = { version = "0.46", default-features = false }` (MIT) with our own content-stream interpreter (`pdf/interpreter.rs`, about 450 lines), after the prototype of 2026-10-23: `pdf-extract` 0.12 drops line widths (`w` and ExtGState `/LW`), colours and mis-sizes Type3 glyphs; `pdf` (pdf-rs) would need the same interpreter with a heavier tree. The prototype's glyph boxes equal pdfplumber's on all 105 pages of the four fixtures within 0.0005 pt. Pin the lopdf minor version (its API breaks at minor releases).
- **D4** (amended 2026-11-10) The manufacturer's demo reports, their vendor JSON exports and the glyph / path dumps are not redistributed and are no longer in the repository. Two own synthetic reports with a generator (`scripts/make_synthetic_session_report.py`) and generator-side expectations are the committed fixtures; the tests against the manufacturer's files are gated on `DBSLOC_MEDTRONIC_FIXTURES` (§9).
- **D5** The generic layer is vendor-neutral by construction (§6); Boston Scientific and Abbott modules follow when example reports are available.
