# 16a — Medtronic Percept session report: PDF layout reference (normative for `specs/16` §7)

**Status:** normative (2026-10-23). Measured on four demo reports of the manufacturer (DEMO MODE, Percept PC and RC, 3389 ring and B33005 directional leads; not in the repository, the folder named by `DBSLOC_MEDTRONIC_FIXTURES` holds them for the gated tests of `specs/16` §9) and verified against their vendor JSON exports (`vendor_json/*.json.gz`, pairing in `pairing.json`, both in that folder). In the four demo reports all 72 stimulation programs, 180 group-history programs, 168 annotations, 184 impedance cells and 16 BrainSense settings match the vendor JSON. The committed fixtures are the two synthetic reports of `specs/16` §9, written from this layout reference; their content is invented and smaller. The sample values quoted below (names, identifiers, serial numbers, dates) are those of the demo reports, kept as the record of what was measured (owner decision 2026-11-10). Coordinates are PDF points with the origin at the top left (`specs/16` §2). The analysis scripts stayed outside the repository; no third-party extractor code was used.


Source material: four synthetic demo reports (`Report_SessionReport_20260930T0905xx.pdf`, Percept PC and RC,
3389 and SenSight B33005 leads) and the four vendor JSON session exports of the same sessions.
Programmer "Application Version: 5.0.676", JSON `DataVersion` "1.2". Everything below was measured on these
files with pdfplumber 0.11.10 / pypdf 6.21.2 plus an own content-stream walker (scripts in
`scratchpad/medtronic/scripts/`). All rules in §3 were re-implemented independently and checked against the
vendor JSON (§3.9): **72/72 stimulation programs, 180/180 group-history programs, 168/168 annotations and
184/184 impedance cells match** with zero unexplained differences.

Conventions used in this document

* Coordinates are PDF points, origin **top-left** (y grows downwards), page 612 × 792.
  `top`/`bottom` = glyph box as pdfplumber reports it; **`baseline`** = 792 − f of the glyph's text matrix
  (what a Rust content-stream interpreter gets directly). See §1.3 for the top/baseline relation per font.
* "PC" = 539 and 558, "RC" = 548 and 606; "3389" = 539 and 548, "B33005" = 558 and 606.
* `x0`/`x1` of a glyph = origin x / origin x + advance width.

---------------------------------------------------------------------------------------------------------

## 0. Pairing PDF ↔ JSON (deliverable 3: `pairing.json` in the folder named by `DBSLOC_MEDTRONIC_FIXTURES`)

PDF header line 3 prints `<device> <serial>` and `Session Date: <local time>`; JSON has
`DeviceInformation.Initial.NeurostimulatorSerialNumber` and `SessionDate` (UTC) plus
`ProgrammerTimezoneId` = `Europe/Zurich`, `ProgrammerUtcOffset` = `+02:00`.
Session date in the PDF = JSON `SessionDate` converted to local time and **truncated** (not rounded) to the
minute (08:57:53 → "8:57 AM", 08:59:46 → "8:59 AM").

| PDF | device / serial | PDF session date (local) | JSON | JSON SessionDate (UTC) | local |
|---|---|---|---|---|---|
| Report_SessionReport_20260930T090539.pdf | Percept PC / NPI_FOLLOWUP_PERCEPT_PC, 3389 | Sep 30, 2026 8:51 AM | Report_Json_Session_Report_20260930T090654.json | 2026-09-30T06:51:45Z | 08:51:45 |
| Report_SessionReport_20260930T090548.pdf | Percept RC / NND_FOLLOWUP_NGRC, 3389 | Sep 30, 2026 8:57 AM | Report_Json_Session_Report_20260930T090647.json | 2026-09-30T06:57:53Z | 08:57:53 |
| Report_SessionReport_20260930T090558.pdf | Percept PC / NPI_FOLLOWUP_PERCEPT_PC, B33005 | Sep 30, 2026 8:59 AM | Report_Json_Session_Report_20260930T090639.json | 2026-09-30T06:59:46Z | 08:59:46 |
| Report_SessionReport_20260930T090606.pdf | Percept RC / NND_FOLLOWUP_NGRC, B33005 | Sep 30, 2026 9:01 AM | Report_Json_Session_Report_20260930T090630.json | 2026-09-30T07:01:41Z | 09:01:41 |

The file-name timestamps are generation/export times (PDF footer prints "Sep 30, 2026 9:05 AM" for
`…T090539.pdf`), not session times; never pair by file name.

---------------------------------------------------------------------------------------------------------

## 1. Document level

### 1.1 Producer, pages, coordinate system

* `/Producer` = `Skia/PDF m112` (Chromium print-to-PDF). No `/Info` title, not encrypted, no tagged structure.
* Every page is 612 × 792 pt (US Letter), `/Rotate` 0. Page counts: 539 = 25, 548 = 25, 558 = 27, 606 = 28.
* Every page content stream starts with `1 0 0 -1 0 792 cm` (y-flip), then `q 0 0 612 792 re W* n` (page clip).
  All drawing is in the page content stream; image XObjects only (no Form XObjects were found).
* Skia emits **one `Tj` per glyph**, each preceded by its own text matrix; glyph x origins are mostly
  integers. Inter-word **spaces are real U+0020 glyphs** (regular font) or U+00A0 (bold font, see 1.2), so word
  segmentation must use the space glyphs, not gaps: advances overlap (e.g. `C` x0 84.000 x1 89.663, next `o`
  x0 89.000), so a gap-based word splitter with tolerance 0 merges/ splits wrongly.

### 1.2 Fonts and encodings

| Resource name (example) | Subtype | BaseFont / FontDescriptor name | Encoding | ToUnicode | Used for |
|---|---|---|---|---|---|
| `/F5` (one per page) | **Type3** | descriptor `/AAAAAA+Roboto`, `/BaseFont` absent | `/Differences` (glyph names `/g0`, `/g5`, …), 1-byte codes | **yes** (bfchar/bfrange, e.g. `<77> → U+00B5`) | all regular text, contact glyphs `. - +`, numbers |
| `/F9` | **Type0 / CIDFontType2**, `Identity-H`, `CIDToGIDMap` present, `/FontFile2` embedded | `BAAAAA+Roboto-Bold` | `Identity-H` 2-byte CIDs | **yes** (`<0062> → U+00A0`, `<02F4> → U+2013`) | headings, labels, section marker tokens |
| `/F15` | Type3 | `/CAAAAA+Roboto_1ital` | Differences | yes | a few numbers on page 1 (e.g. "75%"), 15–25 glyphs per file |
| `/F25`, `/F244` … | Type3 | `/AAAAAA+Roboto` subsets | Differences | yes | per-page extra subsets (e.g. U+03BC μ on survey page) |

* Type3 `FontMatrix` = `[0.00048828125 0 0 -0.00048828125 0 0]` (1/2048, y negated because of the page y-flip).
  Glyph advance = `/Widths[code-FirstChar] × 1/2048 × font size`. Type3 fonts have **no FontBBox**
  (pdfminer warns "Could not get FontBBox"), hence pdfplumber gives `bottom == baseline` for them.
* Unicode mapping must come from the ToUnicode CMaps (glyph names are meaningless `/gNN`). Non-ASCII code points
  present: U+00B5 µ (pulse width "60 µs", 97–113 per file), U+03BC μ (only in survey "μVp" and its rotated axis
  label, 12 in 539), U+2013 – (contact ranges "0–3", "PARESTHESIA–TRANSIENT", "Jul 02 – Sep 30"),
  U+00B0 ° ("Orientation: 120 °", directional only), **U+00A0 NBSP in bold text** (24 per file: every space of
  bold headings such as "Group A (Walking)" is U+00A0 → normalise NBSP to space before regex matching).
* Why the xpdf dump shows "60 �s": `pdftotext` default output encoding is Latin-1. U+00B5 is written as byte
  0xB5 and U+2013 is mapped to 0xAD (soft hyphen); both are invalid UTF-8 and show as U+FFFD in a UTF-8 viewer.
  U+03BC is not representable in Latin-1 and is **silently dropped**: the dump prints "Peak Magnitude: 1.965820313
  Vp" although the PDF says "μVp". Use `pdftotext -enc UTF-8` (or the ToUnicode mapping) — the PDF itself is fine.

### 1.3 Glyph box vs baseline (needed to reuse the coordinates in this file)

| font | size | pdfplumber `top` → baseline | example |
|---|---|---|---|
| Roboto (Type3) | any | baseline = top + size (= bottom) | "Parkinson's Disease" 12 pt top 138.00 → baseline 150 |
| Roboto-Bold (CID) | any | baseline = top + 0.7292·size | "Diagnosis:" 12 pt top 141.25 → baseline 150; 18 pt top 98.88 → 112; 6.7 pt top 92.12 → 97 |

So a bold label and its regular value on the same visual line share the **baseline** (150) but not the top.
Group lines by baseline (tolerance ±1 pt), never by top.

### 1.4 Colours, alpha, invisible text and line widths

Text can be invisible in three distinct ways (measured by aligning `Tj` operators with glyphs; script `alpha.py`):

| what | fill colour | fill alpha (`/ca` via ExtGState) | where |
|---|---|---|---|
| section marker tokens `<s-xx>` / `<e-xx>` | white 1,1,1 | 1 | x = 54, Bold 6.7 pt (§1.6) |
| off-contact glyph `.` | white | 1 | inside every non-active contact circle |
| empty-program-slot placeholder `--` | white | **0.0039** (`/G39`) | top of an unused program slot |
| header "Session Report", "DEMO MODE", "END OF REPORT", "Detected/Selected Frequency:", "LFP Magnitude:", usage-chart letters/percent labels | black | **0** (`/G4`) | the visible copy is drawn as **vector glyph outlines** (filled paths; "DEMO MODE" outlines are red 1,0,0); the alpha-0 text is the extractable copy |
| usage legend "Group A…D" | black | 0.2 / 0.298 / 0.502 / 0.8 | group-usage legend |

pdfplumber ignores `/ca` and reports these as plain black, and pdfminer **ignores `/LW` in ExtGState**
(pdfplumber `linewidth` is 0 for every path). Line widths are set only through ExtGState `gs`:
`/G8 LW 3` (header rule), `/G16 LW 1`, `/G23 LW 0.30000001`, `/G24 LW 0.40000001`, `/G38 LW 2`, `/G12 LW 0`.
A Rust extractor must track `q/Q/cm/w/gs` itself and scale LW by √|det(CTM)| (CTM scale is 1 here).

### 1.5 Header and footer (every page)

| item | font | baseline | x | text / regex |
|---|---|---|---|---|
| title | Roboto 12, alpha 0 (+ outline paths) | 35 | 54.0 | `Session Report` |
| demo flag | Roboto 12, alpha 0 (+ red outlines) | 35 | 271.5–340.3 | `DEMO MODE` (absent in non-demo reports — treat as optional) |
| logo | image 324×89 px | box (477, 24)–(558, 46.25) | | Medtronic logo |
| patient line | Roboto 12 | 53 | 54.0 | `^(?P<name>.+) (?P<pid>\S+)$` — "Demo Patient MRN:123456" = First Last + PatientId; the split is only safe because the patient ID has no space; prefer the page-1 values |
| app version | Roboto 12 | 53 | right aligned, x1 ≈ 557.7 | `Application Version: (\S+)` |
| device line | Roboto 12 | 71 | 54.0 | `^(Percept (?:PC|RC)) (\S+)$` (device name + serial) |
| session date | Roboto 12 | 71 | right aligned, x1 ≈ 558.5 | `Session Date: (Mon d, yyyy h:mm AM|PM)` |
| header rule | line LW 3 | y = 77 | 54–558 | |
| footer rule | line LW 1 | y = 772 | 54–558 | |
| print time | Roboto 12 | 784 | 54.0 | `^[A-Z][a-z]{2} \d{1,2}, \d{4} \d{1,2}:\d{2} [AP]M$` (report generation time) |
| page number | Roboto 12 | 784 | right aligned, x1 ≈ 557.7 | `^Page (\d+)$` (no "of N") |

Body content lies between y ≈ 80 and y ≈ 760. Section titles are Roboto-Bold 18 pt at x = 54 followed by a
hairline (LW 0) rule from the title's end to x = 558 at y = baseline − 7 (e.g. "STIMULATION SETTINGS"
baseline 117, rule y 110 from x 257; "PATIENT PROGRAMMER SETTINGS" baseline 101, rule y 94 from x 340).

### 1.6 Hidden section marker tokens

Literal text, Roboto-Bold 6.7 pt, white fill, alpha 1, always at x0 = 54.0, own baseline line
(e.g. `<s-pi>` baseline 97 on page 1). Regex: `<([se])-(?:s-)?([a-z]+)>`.

| start | end | section (visible title) | pages 539 / 548 / 558 / 606 |
|---|---|---|---|
| `<s-pi>` | `<e-pi>` | patient information (name, diagnosis, ID, gender, DOB, notes) | 1 / 1 / 1 / 1 |
| `<s-di>` | `<e-di>` | DEVICE | 1 |
| `<s-ss>` | `<e-ss>` | STIMULATION SETTINGS + lead configuration + groups | 2–7 (all) |
| `<s-pps>` | `<e-pps>` | PATIENT PROGRAMMER SETTINGS | 8–9 |
| `<s-gs>` | `<e-gs>` | GROUP SETTINGS | 10–11 |
| `<s-sii>` | `<e-sii>` | SYSTEM INTEGRITY INFORMATION (impedance) | 12 / 12 / 12–13 / 12–13 |
| `<s-gh>` | `<e-gh>` | GROUP HISTORY | 13–15 / 13–15 / 14–16 / 14–16 |
| `<s-gu>` | `<e-gu>` | GROUP USAGE | 16 / 16 / 17 / 17 |
| `<s-rc>` | `<e-rc>` | BATTERY RECHARGE — **RC only** | – / 17 / – / 18 |
| `<s-sa>` | `<e-sa>` | SESSION ANNOTATIONS | 17–20 / 18–21 / 18–23 / 19–24 |
| `<s-bss>` | `<e-bss>` | Electrode Survey (BrainSense survey) | 21–22 / 22 / 24 / 25 |
| `<s-mbss>` | `<e-mbss>` | Electrode Identifier | 23 / 23 / 25 / 26 |
| `<s-pe>` | `<e-pe>` | PATIENT EVENTS | 24 / 24 / 26 / 27 |
| `<s-es>` | **`<e-s-es>`** | Event Summary (+ "END OF REPORT") | 25 / 25 / 27 / 28 |

Pitfalls:
* The end token of the event summary is `<e-s-es>`, not `<e-es>`.
* A continuation page may **repeat the start token** without an end token on the previous page
  (558/606 page 13 starts with `<s-sii>` again; page 12 has no `<e-sii>`).
* Pages without any token belong to the section of the last start token (e.g. 539 pages 3–6).
* The tokens' y positions are not reliable section boundaries: `<e-di>` (y 651) precedes the
  "Feature Information / Code" block (y 653–697) that belongs to the device section; `<e-gu>` sits in the middle of
  the usage chart; `<e-bss>` of 539 is alone at the top of page 22. Use tokens only to classify pages, and parse
  content by position inside the page.
* Page 1 holds both `pi` and `di`.

Recommended page classification: walk pages in order; section = last start token on the page if any, else the
section carried over from the previous page (reset to none after an end token that is the last token on a page).

---------------------------------------------------------------------------------------------------------

## 2. Sections, fields and the matching JSON paths

General text rules (all sections): build lines by **baseline** (±1 pt) and font size (±0.6); inside a line,
start a new text run where the gap to the previous glyph exceeds ~6 pt (columns share baselines); join glyphs
with the space glyphs they carry; NBSP → space. Drop white glyphs (`. -- <markers>`) from text lines, and drop
glyphs whose centre lies inside a contact circle (they are polarity marks, §3). Values in this section are given
for 539 (PC) unless stated; RC differs only where noted. `J.` = JSON root; `{S}` = `Initial` or `Final`.

### 2.1 Patient information (page 1, `pi`)

| printed | font | position (baseline, x) | value format | JSON |
|---|---|---|---|---|
| name (section title) | Bold 18 | 112, 54 | display name "Demo Patient" (First + " " + Last) | `J.PatientInformation.{S}.PatientFirstName` + `PatientLastName` |
| `Diagnosis:` → value | Bold 12 label at x 95, value Regular 12 at x 156 | 150 | "Parkinson's Disease" | `…Diagnosis` = `DiagnosisTypeDef.ParkinsonsDisease` |
| `Patient ID:` → value | label x 95, value x 156 | 170 | "MRN:123456" (free text, may contain ":") | `…PatientId` |
| `Gender:` → value | label x 247, value x 293 | 170 | "Male" | `…PatientGender` = `PatientGenderDef.MALE` |
| `Date Of Birth:` → value | label x 339, value x 416 | 170 | "Jun 25, 1959" (`Mon d, yyyy`) | `…PatientDateOfBirth` = `1959-06-24T23:00:00Z` (local midnight, see §6) |
| `Clinician Notes:` | label x 95 baseline 190 | value x 95, next line(s) from baseline 206 | free text, possibly multi-line, until `<e-pi>` / "DEVICE" | `…ClinicianNotes` |

Locate values label-relative: value = text run on the same baseline starting right of the label's x1 and left of
the next label; Clinician Notes = all lines below the label down to the DEVICE title (y 263). A 32×62 px icon is
drawn at (54, 137). JSON has Initial and Final copies (identical here); only one is printed.

### 2.2 Device (page 1, `di`)

Three-column grid of **Bold 12 labels with the value on the next line (Regular 12, baseline +19)**. Column x
differs between PC (115 / 270 / 450) and RC (120 / 275 / 455) — locate by label, not by absolute x. Two labels
wrap: "Product Application" / "ID:" (baselines 347/363, value at 382) and, on PC, "Estimated Battery Remaining"
followed by a regular-weight line "(based on last 7 days)" (611) and the value (630). A device picture (46×62 px PC,
51×62 px RC) sits at (54, 283).

| label (bold) | value example (PC / RC) | JSON (`J.DeviceInformation.{S}.*` unless noted) | note |
|---|---|---|---|
| `NEUROSTIMULATOR` | "Percept PC" / "Percept RC" | `Neurostimulator` | |
| `Serial Number` | "NPI_FOLLOWUP_PERCEPT_PC" | `NeurostimulatorSerialNumber` | |
| `Implanted` | "Mar 4, 2026" | `ImplantDate` "2026-03-04T07:51:41Z" | local date of the UTC timestamp |
| `Product Application ID:` | "0000" | `ProductApplicationId` "00.00" | **format differs** (PDF drops the dot) |
| `Product Version:` | "07.00.00" / "01.00.00" | `ProductVersion` | |
| `Core Application ID:` | "0000" | `CoreApplicationId` "00.00" | dot dropped |
| `Core Version:` | "00.00.00" | `CoreVersion` | |
| `Device Application ID:` | "0000" | `DeviceApplicationId` "00.00" | dot dropped |
| `Device Version:` | "00.00.00" | `DeviceVersion` | |
| `Model Number` | "B35200" / "B35300" | `NeurostimulatorModel` | |
| `Device Name` | "--" | `DeviceName` "" | empty string printed as `--` |
| `Components` | "Left Chest" | `NeurostimulatorLocation` `InsLocation.CHEST_LEFT` | |
| `Battery Level` | "75%" / "80%" (font Roboto_1ital on PC) | `J.BatteryInformation.BatteryPercentage` | RC JSON also has `NonRechargeBatteryStatus.BatteryLevelPercent` 75 — **not** the printed value |
| PC: `Estimated Battery Remaining` + "(based on last 7 days)" | "4 years, 1 month" | `J.BatteryInformation.EstimatedBatteryLifeMonths` 49 | regex `(\d+) years?, (\d+) months?` |
| RC: `ERI Date` | "Feb 28, 2041" | `J.BatteryInformation.ERIDate` "2041-02-28T08:01:37Z" | |
| `Feature Information` / `Code` | "160d" | `J.FeatureInformationCode` | printed after `<e-di>` |

Not printed: `DeviceDateTime*`, `OverdischargeCount`, `AccumulatedTherapyOnTime*`, `BatteryInformation.TTEData`,
`BatteryStatus`, all of `NonRechargeBatteryStatus`, `ProgrammerTimezone*`, `ProgrammerLocale`, `SessionEndDate`,
`DataVersion`, `AbnormalEnd`, `FullyReadForSession`, `ReadAllStatus`. `ProgrammerVersion` = header
"Application Version".

### 2.3 Stimulation settings header (page 2, `ss`)

Column headers "Initial" (x 178) and "Final" (x 420), baseline 148. Row labels Regular 12 at x 80 (wrapping over
several lines), Initial values at x 184, Final values at x 424, on the baseline of the label's first line.

| label | baseline | value | JSON |
|---|---|---|---|
| `STIMULATION` | 173 | `On` / `Off` | `J.Stimulation.InitialStimStatus` / `FinalStimStatus` (`StimStatusDef.ON`) |
| `CHECK BATTERY` `REMINDER` | 192 | "11:00" (24 h `HH:mm`; presumably "Off" when disabled — not in samples) | `J.BatteryReminder.ReminderTime` (+ `Enabled`); only one JSON value, printed in both columns |
| `CLOSE` `COMMUNICATION` `REQUIRED FOR` `THERAPY OFF` | 225 | `ON` | **not in JSON** |

### 2.4 Lead configuration (page 2, `ss`)

"Lead Configuration" Bold 12 at (80, baseline 348). Four boxes 106 × 170 (stroked, LW 0.3), top 378, x0 = 74,
194 (Initial left / right) and 329, 449 (Final left / right); snapshot = Initial if box x0 < 306. Above each box a
29×29 px hemisphere icon (11.6 pt, ◑ left / ◐ right) and Regular 12 text `^(Left|Right) (\S+)$` at baseline 372
("Left STN" — the second word is the target, JSON `LeadLocation` `LeadLocationDef.Stn`). Inside each box:

* the lead drawing (labelled 16 × 16 pt circles, see §3.1) with the contact labels printed **inside** the circles
  (Regular 6.7) — this is the ground truth for the label layout of that lead;
* 8.7 pt lines at x = box.x0 + 10, baselines 484 / 494 / 504:
  `Contacts: (\d+)–(\d+)` (U+2013), `Lead Model: (\S+)`, and for directional leads `Orientation: (\d+) °`
  (space, U+00B0).

| printed | JSON `J.LeadConfiguration.{S}[i]` |
|---|---|
| hemisphere word | `Hemisphere` `HemisphereLocationDef.Left/Right` |
| target word | `LeadLocation` (`LeadLocationDef.Stn`) |
| `Contacts: 0–3` / `8–11` / `0–7` / `8–15` | `ElectrodeNumber` `InsPort.ZERO_THREE` / `EIGHT_ELEVEN` / `ZERO_SEVEN` / `EIGHT_FIFTEEN`; first number = `TipOffset` (`TipOffsetDef.ZERO` / `EIGHT`) |
| `Lead Model: 3389` / `B33005` | `Model` `LeadModelDef.LEAD_3389` / `LEAD_B33005` |
| `Orientation: 120 °` | `OrientationInDegrees` (directional only; absent for 3389) |

Contact labels: ring lead with range a–b → levels a, a+1, a+2, a+3. Directional lead with range a–(a+7) →
`a`, `(a+1)a`, `(a+1)b`, `(a+1)c`, `(a+2)a`, `(a+2)b`, `(a+2)c`, `a+3` (e.g. right: 8, 9a, 9b, 9c, 10a, 10b,
10c, 11). JSON electrode names are **always 0-based** (`FourElectrodes_0..3`, `SenSight_0`, `SenSight_1a`,
`Sensight_1b`, `Sensight_1c`, `SenSight_2a`, `Sensight_2b`, `Sensight_2c`, `SenSight_3` — note the inconsistent
capitalisation `SenSight`/`Sensight`), so PDF label = JSON level + tip offset (0 left, 8 right here). No B33015 lead
occurs in the samples; it shares the B33005 contact geometry (assumed, unverified).

### 2.5 Stimulation groups A–D (pages 2–7, `ss`) — text part (geometry in §3)

Per page, two columns: Initial (boxes x0 = 60) and Final (x0 = 315), each box 221 wide.

* Group header: Bold 12 at x = box.x0, `^Group ([A-D])(?: \((.*)\))?$` (NBSP spaces!), baseline = box.top − 34
  for the first box of the group on the page. The group name in parentheses = JSON `Groups.{S}[i].GroupName`.
  A box without a header above it on the same page continues the previous group of that column (Group D right
  hemisphere on page 7).
* Hemisphere line: icon + Regular 12 `^(Left|Right) (\S+)$`, baseline = box.top − 10, centred over the box.
* Box height: 215 for regular groups; BrainSense group D boxes are taller: 485 with the ADAPTIVE SETUP block, 278 for "Sensing Only" (539 Final left).
* **Active group = box border line width 2.0** (`/G38`), inactive = 0.3 (`/G23`). The same LW is applied to the
  program-slot separator inside the box. Exactly one group per snapshot is active; both hemisphere boxes of the
  active group are thick. JSON `Groups.{S}[i].ActiveGroup`. (pdfplumber cannot see this, §1.4.)

Text inside a program slot (8.7 pt Regular unless stated), located relative to the slot's shaft (§3.2):

| printed | regex | example | JSON (`Groups.{S}[g].ProgramSettings…`) |
|---|---|---|---|
| amplitude | `^(\d+\.\d) (mA|V)$` | "2.9 mA" | `{Left,Right}Hemisphere.Programs[p].AmplitudeInMilliAmps`; for BrainSense programs `SensingChannel[h].SuspendAmplitudeInMilliAmps` (no `AmplitudeInMilliAmps` exists there). Unit V ⇔ `AmplitudeControl` VOLTAGE (only mA seen; `ProgramSettings.AmplitudeControl` = `AmplitudeTypeDef.CURRENT`) |
| pulse width | `^(\d+) [µμ]s$` | "60 µs" | `…PulseWidthInMicroSecond` |
| rate | `^(\d+) Hz$` | "130 Hz" | `ProgramSettings.RateInHertz` (group level) **or** per program `Programs[p].RateInHertz` (group C uses per-hemisphere rates 150/100) or `SensingChannel[h].RateInHertz` |
| per-contact current | `^(\d{1,2}[abc]?): (\d+\.\d) (mA|V)$` | "2: 0.8 mA", "10a: 1.0 mA" | `ElectrodeState[k].ElectrodeAmplitudeInMilliAmps` of the contact with that label (§3.6) |
| BrainSense mode (D only, 8 pt) | `^BrainSense: (.+)$` | "Adaptive", "Sensing Only" | Adaptive ⇔ `SensingChannel[h].AdaptiveTherapyStatus` ≠ `NOT_CONFIGURED`; "Sensing Only" ⇔ `ADBSStatusDef.NOT_CONFIGURED` (also no `Mode` key) |
| adaptive status (D, 8 pt) | `^Adaptive Therapy: (.+)$` | "Adapting", "Paused" | `AdaptiveTherapyStatus` `RUNNING` → Adapting, `SUSPENDED` → Paused; line absent for Sensing Only |

BrainSense (group D) box, below the program text (all label-relative, Regular):

| printed | font | JSON `…SensingChannel[h].` |
|---|---|---|
| `SENSING SETUP` heading | 12 | |
| `LFP Frequency: 24.41 Hz` | 8.7 | `SensingSetup.FrequencyInHertz` |
| `THRESHOLD MODE` / `Mode: Dual Threshold` | 12 / 8.7 | `Mode` `AdaptiveModeDef.DUAL_THRESHOLD_DIRECT` |
| `Upper LFP Threshold:` (8.7) with the value on the next line (**12 pt**, same x) and `Captured: 30` below | | `UpperLfpThreshold` 50.0 / **`MeasuredUpperLfp`** 30 |
| `Lower LFP Threshold:` (second column, x +87) | | `LowerLfpThreshold` / **`MeasuredLowerLfp`** |
| `ADAPTIVE SETUP` (absent for Sensing Only) | 12 | |
| `Maximum Limit:3.3 mA` (no space after colon) | 8.7 | `UpperLimitInMilliAmps` |
| `Paused Amplitude: 2.9 mA` | **10** | `SuspendAmplitudeInMilliAmps` |
| `Minimum Limit:2.5 mA` | 8.7 | `LowerLimitInMilliAmps` |
| `Transition Duration:` / `Transition Up: 2.3 mins` / `Transition Down: 5.0 mins` | 8.7 | `TransitionUpInMilliSeconds` 138000 / `TransitionDownInMilliSeconds` 300000 (ms / 60000, one decimal) |

The adaptive-limit icons (76×59 px) and the coloured transition curve (vector lines, colours 0.06/0.51/0.78 and
0.73/0.41/0.78) are decoration. Not printed: `UpperCaptureAmplitudeInMilliAmps`, `LowerCaptureAmplitudeInMilliAmps`
(the printed "Captured:" values are `MeasuredUpper/LowerLfp`), `BrainSensingStatus`,
`SensingSetup.ChannelSignalResult` (PSD arrays), `ProgramId`, `ElectrodeFractionOf64`, case polarity. When
"Sensing Only" the JSON still carries limits and transition times that are not printed.

### 2.6 Patient programmer settings (`pps`, pages 8–9)

Columns Initial (x ≈ 74–80) / Final (x ≈ 328–334); "Initial" x 151 / "Final" x 405 at baseline 132. Per group:
Bold 12 `Group X (Name)`, per hemisphere Regular 12 `Left STN`, then two 8.7 pt columns:

| left column (x 74 / 328) | right column (x 152 / 406) | JSON `Groups.{S}[g]` |
|---|---|---|
| `Mode` (bold) / next line `Advanced Adjust` | `Parameter: Adjust Amplitude` (bold) | `Mode` `LimitModeDef.AdvanceEdit`, `AdjustableParameter` `LimitTypeDef.Amplitude` |
| `Mode` / `Advanced View` | `Parameter: View Amplitude` | `LimitModeDef.AdvanceView` |
| `Mode` / `Adaptive Therapy` | `Parameter:` then `Paused Amplitude: x mA`, `Adapting between`, `Maximum Limit: x mA`, `Minimum Limit: x mA` | BrainSense: `SensingChannel[h].SuspendAmplitudeInMilliAmps`, `UpperLimitInMilliAmps`, `LowerLimitInMilliAmps` (JSON `Mode` still says AdvanceEdit) |
| | `Program 1: 2.5 mA to 3.3 mA` / `Value : 2.9 mA` (space before colon) | `Programs[0].LowerLimitInMilliAmps` / `UpperLimitInMilliAmps` / `AmplitudeInMilliAmps`; printed only if the program has limits (a Final program without limits prints nothing, e.g. 539 Final A left) |

Regex: `^Program (\d+): (\d+\.\d) (mA|V) to (\d+\.\d) (mA|V)$`, `^Value : (\d+\.\d) (mA|V)$`.
Group D Final "Sensing Only" in 539 still prints "Paused Amplitude: 2.9 mA" only.

### 2.7 Group settings (`gs`, pages 10–11)

Columns x 60 (Initial) / 329 (Final), each group block framed by a 241-wide rectangle (LW 2, not an
active marker). Per group: Bold 12 header, then 8.7 pt lines; BrainSense group adds per-hemisphere blocks
(Bold 12 `Left STN`) that can continue on the next page.

| printed | JSON |
|---|---|
| `SoftStart/Stop: 4 Seconds` | `Groups.{S}[g].GroupSettings.SoftStartStop.DurationInSeconds` (+ `Enabled`) |
| `Cycling: Off` | `GroupSettings.Cycling.Enabled` false |
| `High Pass Filter: 1 Hz` | `GroupSettings.HighPassFilterInHertz` (group level, printed per hemisphere) |
| `Averaging Duration: 1.2 secs` | `SensingChannel[h].SensingSetup.AveragingDurationInMilliSeconds` 1200 (606 right: 3.0 secs) |
| `Sensing Blanking Duration: 1.4 ms` | `GroupSettings.SensingBlankingDurationInMicroseconds` 1390 → **rounded** to 0.1 ms |
| `Onset Duration:` / `Upper: 1.2 secs` / `Lower: 1.2 secs` | `SensingChannel[h].AdaptiveTherapy.UpperThresholdOnsetInMilliSeconds` / `LowerThresholdOnsetInMilliSeconds` |
| `Detection Blanking Duration: 2.0 secs` | `…AdaptiveTherapy.DetectionBlankingDurationInMilliSeconds` |
| `Adaptive Startup Delay: 10.0 secs` | `…AdaptiveTherapy.AdaptiveStartupDelayInMilliSeconds` |

The xpdf `-layout` dump interleaves the two columns differently on each page (e.g. 539 page 10/11); positional
parsing (x < 306 → Initial) is reliable.

### 2.8 Impedance (`sii`, page 12, or 12–13 for directional leads)

* "ELECTRODE IMPEDANCE" Bold 12 at (54, baseline 134); `^Impedance measured at (.+)$` Regular 12 baseline 158
  → `J.Impedance[0].TestCurrentMA` ("Automatic increase").
* 3389: both hemispheres on one page, "Left STN" at x 110 and "Right STN" at x 344 (baseline 182), separated by a
  vertical line x 283 (LW 1.5). B33005: Left on page 12, Right on page 13 (each full width, heading at x 110/120).
* Table titles "MONOPOLAR" (x 54/303) and "BIPOLAR" (x 196/446), 8.7 pt.
* Each measurement is a **35 × 35 pt cell** (stroked rect, LW 0), containing a 62×41 px status icon (check
  mark; 14.26 × 9.43 pt at cell.x0+10, cell.top+10) and the value (8.7 pt, baseline cell.top + 33), format
  `^[<>]?\d{1,3}(,\d{3})*$` ohm (comma thousands separator).
* Row label (8.7 pt) left of the table on baseline cell.top + 28: monopolar at x 54 (303 on the right half),
  bipolar at table.x0 − 15 (x 145 / 395). Take the nearest label to the left on that baseline.
* Bipolar column labels on the baseline (first cell row top − 7), horizontally centred over the cell.
* Monopolar = case ↔ row contact. Bipolar = row contact ↔ column contact; strictly lower-triangular (column
  label < row label). Row order top→bottom: 3,2,1,0 (or 11..8); directional: 3, 2c, 2b, 2a, 1c, 1b, 1a, 0, columns
  0, 1a, 1b, 1c, 2a, 2b, 2c → 8 monopolar + 28 bipolar cells per directional lead, 4 + 6 per ring lead.
* JSON: `J.Impedance[0].Hemisphere[h].SessionImpedance.Monopolar[k]` (`Electrode1` = `ElectrodeDef.Case`,
  `Electrode2`, `ResultValue`) and `.Bipolar[k]` (`Electrode1` < `Electrode2`). Verified: all 184 cells
  (20 + 20 + 72 + 72) equal. `J.Impedance[0].ImpedanceStatus` (`GOOD`) is not printed as text; each cell has
  an icon instead (only the "good" icon occurs).

### 2.9 Group history (`gh`, 3 pages)

Two columns of dated sessions per page. Date headings Regular 12 `^Mon d, yyyy$` at x 60 (left column) /
x 329 (right column), baseline = first box top − 14 (134). Under a date: four row boxes 240 × 110 (LW 0.3), one per
group, at x0 = 75 / 344, stacked with no gap (tops 146, 256, 366, 476). Group letter Bold 12 `^[A-D]$` at x 60 / 329,
baseline = box.top + 58.

**A date heading can be orphaned at the bottom of a page**: 539 page 13 ends with "Jun 2, 2026" (x 60) and
"May 3, 2026" (x 329) at baseline 627 with no box below; their four row boxes are on page 14, which carries no date
heading at all; page 15 starts with "Apr 3, 2026" (x 60, baseline 102). Rule: date of a box = the last date
heading in the same column in reading order (page, baseline), carried across page breaks. Columns are
independent (left column holds the 1st, 3rd, 5th… session, right column the 2nd, 4th…); sessions are printed
newest first (= `GroupHistory` order).

Each box has four 60-pt slots: slot i spans x0 + 60·i … x0 + 60·(i+1); **slots 0–1 = left hemisphere programs 1–2,
slots 2–3 = right hemisphere programs 1–2**. Vertical separators (h 88, LW 0.3) at x0 + 60/120/180 are drawn only
between used slots. Assign a shaft to slot ⌊(shaft.x0 − box.x0)/60⌋ (shaft x0 offsets seen: 29 regular, 21 for
BrainSense diagrams). Per slot: lead diagram (§3) and amplitude / pulse width / rate (8.7 pt) below the diagram;
BrainSense slots additionally print 7 pt "BrainSense:", "Adaptive", "Adaptive Therapy:", "Adapting" to the right of
the diagram, spilling into the neighbouring (empty) slot. No per-contact currents, no limits, **no active-group
marker** (all history boxes LW 0.3) — JSON `GroupHistory[n].Groups[g].ActiveGroup` is not printed.

JSON: `J.GroupHistory[n]` (`SessionDate` UTC → printed local date), `.Groups[g]` with the same
`ProgramSettings` structure as `Groups` (without `ElectrodeAmplitudeInMilliAmps`). Historical diagrams must be
labelled with the session's lead configuration (no historical lead data printed). Verified 180/180 programs.

### 2.10 Group usage (`gu`, 1 page)

Chart only. Legend "Group A"…"Group D" (Regular 12, x 515, baselines 131/150/169/188; text alpha 0.2/0.3/0.5/0.8
encodes the colour), y ticks "100…20" (10 pt, x ≈ 76–94), rotated y-axis title "Time Used (%)", month labels
`^[A-Z][a-z]{2} \d{4}$` (10 pt, baseline 535) under stacked vector bars (filled rects 50 pt wide), month totals
"97%", "100%", "94%" (15 pt) above the bars, segment letters (15 pt, alpha 0). A full-page 612×792 px raster image
is also placed on this page. JSON `J.GroupUsagePercentage[g].UsagePercentage` holds overall percentages only
(e.g. 26/13/… for 539); the monthly breakdown printed in the PDF is not in the JSON and vice versa.

### 2.11 Battery recharge (`rc`, RC only, 1 page)

"BATTERY RECHARGE" title; rotated y title "Battery Recharge (%)"; ticks 100…20; one vertical bar (filled rect
27 pt wide, height 3.4 pt per %) per recharge session for the **10 most recent** sessions, oldest left; x labels
`^\d{2}/\d{2}$` (MM/DD, no year, 10 pt, baseline 495); 6 pt labels inside the bar's x range: start % (lower), end %
(upper) and duration ("31 min" or "1.0 hr"). METRICS block: "Excellent" (14 pt) / "Recharge Coupling",
"70%" / "Avg. Ending", "40 min" / "Avg. Recharge Time", "2.9 days" / "Avg. Recharge Interval" (value line above
its caption); COUPLING legend Excellent / Good / Fair (bar colour = coupling).
JSON `J.RechargeCount[i]` (30 sessions: `SessionStartDate`, `StartingPercentage`, `EndingPercentage`,
`DurationInHours`, `RechargeCoupling`); `J.BatteryInformation.AverageRechargeCoupling`,
`AverageEndingBatteryPercentage`, `AverageRechargeTimeInMinutes`, `AverageRechargeIntervalInMinutes` (4160 →
"2.9 days"). Durations < 1 h are printed as rounded minutes (0.63 h → "38 min").

### 2.12 Session annotations (`sa`, 4–6 pages) — stimulation trials / symptom ratings

* Hemisphere block heading Bold 12 `^(LEFT|RIGHT) HEMISPHERE$` at x 60; "CONFIGURATION" 6.7 pt.
* Column headers are **rotated 90°** text (text matrix `[0 1 −1 0 …]`, pdfplumber `upright = False`, reported
  sizes vary 1.7–5.9 although it is one font size) at x0 = 195.3, 256.3, 317.3, 378.3, 439.3 (pitch 61), reading
  **bottom→top** (sort glyphs by descending y-from-top). Whitespace inside may be doubled: normalise.
  Headers seen: TREMOR, BRADYKINESIA, MOOD ALTERATION, MUSCLE CONTRACTIONS, PARESTHESIA–TRANSIENT
  ⇔ `SymptomEffectDef.SP_PD_Tremor`, `SP_PD_Bradykinesia`, `SE_MoodAlteration`, `SE_MuscleContractions`,
  `SE_ParesthesiaTransient`.
* One row per annotation, anchored on the date line (6.7 pt `^Mon d, yyyy$` at x 65); following lines at the
  same x: amplitude `^(\d+\.\d\d) (mA|V)$` (two decimals!), `^(\d+) µs$`, `^(\d+) Hz$` (baselines +11, +24, +37).
  Row pitch 52 (date baselines 231, 283, …); rows continue across pages; some rows (roughly every other one) are
  framed by a 492 × 50 rect at x 60–552 — do not rely on the frames, anchor on the date lines. A vertical
  hairline at x 170 separates configuration and scores.
* Small lead diagram per row (§3.7): 6 × 6 circles, shaft 2 × 30.
* Score per column: 6.7 pt text centred near the column x (± 8 pt): a digit = rating (bar 25 × 5 outline with a
  filled part 2.5 pt per rating point), `-` = symptom not part of this annotation. **A printed "0" corresponds to
  JSON `RatingOption10Def.Rating_None`** (JSON never uses Rating_0); `Rating_n` → "n".
* JSON `J.Annotations[i]`: `Hemisphere`, `Date` (UTC, local midnight), `RateInHertz`, `AmplitudeControl`,
  `Program[0].ElectrodeState[]` (only `"Negative"` states, value without the `ElectrodeStateDef.` prefix),
  `Program[0].AmplitudeInMilliAmps`, `PulseWidthInMicroSecond`, `Symptoms[]`, `SideEffects[]`.
  **Order differs**: the PDF sorts per hemisphere by contact level ascending (rings first for directional, then
  single segments a, b, c) and amplitude ascending; JSON lists them in another order. Match as a multiset.
  Verified 168/168 (34 + 34 + 50 + 50).

### 2.13 BrainSense Electrode Survey (`bss`) and Electrode Identifier (`mbss`)

If no data: Bold 12 "No Data Available" at (70, 143) (548, 558, 606). 539 (PC 3389) has data:

* Survey: hemisphere Regular 14 "Right STN" at (71, 133); plots in a 3-column grid (column pitch 170, row pitch
  150), each a 140 × 80 frame with a PSD curve (vector), rotated y label "LFP Magnitude (μVp)", x label
  "LFP Frequency (Hz)". Text (7.5 pt) at x = 60 / 230 / 400, baselines 237 / 247 / 257 (+150 per row):
  `^Sensing Electrodes: E(\d+[abc]?) - E(\d+[abc]?)$`, `^Peak Magnitude: (\d+\.\d{9}) μVp$` (U+03BC),
  `^Peak Frequency: (\d+\.\d\d) Hz$`.
  JSON `J.LFPMontage[i]` (`SensingElectrodes` `SensingElectrodeConfigDef.ZERO_AND_THREE` ⇔ "E11 - E8": higher
  contact printed first, + tip offset; `PeakMagnitudeInMicroVolt` 1.9658203125 → printed rounded to 9 decimals;
  `PeakFrequencyInHertz`) and `J.BrainSenseSurveys[k]` with `SurveyMode` "ElectrodeSurvey" (`PeakMagnitudeInMicroVoltRMS`
  — same numbers although named RMS while the PDF says Vp). Left-hemisphere survey entries with all zeros are not
  printed. PSD arrays (`LFPFrequencyinHertz`, `LFPMagnitudeinMicroVoltPeak`) only as plots.
* Identifier: legend "Most LFP Magnitude" / "Medium LFP Magnitude" / "Least LFP Magnitude" (144×59 px swatches),
  "Right STN" 14 pt, then 8.7 pt label/value pairs on consecutive lines at x 70: "Detected Frequency:" → "22.46 Hz",
  "Selected Frequency:" → "22.46 Hz", "LFP Magnitude:" then rows `^(\d+[abc]?) (Most|Medium|Least)$` (contact at
  x 80, rank at x 100). Labels are alpha-0 text over vector outlines. JSON `J.BrainSenseSurveys[k]`
  (`SurveyMode` "ElectrodeIdentifier") `.ElectrodeIdentifier[e]`: `PeakFrequencyInHertz` (Detected),
  `SelectedFrequencyInHertz`, `SensingElectrodes` `ELECTRODE_ONE_RING` (+ tip offset), `RankingatSelectedFrequency`
  `HIGHEST_RANK` → Most, `LOWEST_RANK` → Least (Medium assumed for the middle rank). Not printed:
  `PeakMagnitudeInMicroVoltRMS`, `ReferenceHemisphere`, `ReferenceElectrode`, `ArtifactStatus`.

### 2.14 Patient events (`pe`)

Columns Initial (labels x 90, values x 167) / Final (x 345 / 422), "Initial" x 164 / "Final" x 420 at
baseline 148. Each event is framed (226 × 45 rect with behaviour, 226 × 30 without, LW 2). 8.7 pt:
`^Name:` + value on the same baseline at the value column, optional `^Additional Behavior:(\S+)$` (no space after
colon) on the next line. JSON `J.PatientEvents.{S}[i].EventName` and `."Additional Behavior"` (key contains a
space). Order identical.

### 2.15 Event summary (`es`)

Regular/Bold 12, two columns: labels x 54 / 335, values right-aligned ending x ≈ 251.8 / 532.8 (Bold headers:
`Events` + date range, `Group D Left STN` + date range). Date range `^([A-Z][a-z]{2}) (\d{2}) – ([A-Z][a-z]{2})
(\d{2})$` (zero-padded day, U+2013, no year) ⇔ `J.EventSummary.SessionStartDate` / `SessionEndDate` (local dates).
Rows: event name + count ⇔ `J.EventSummary.Events[i].EventName` / `EventCount`; per hemisphere
"Above Threshold" `AboveThresholdPercent`, "Between Thresholds" `BetweenThresholdPercent`, "Below Threshold"
`BelowThresholdPercent`, "Average Amplitude" (no unit, **2 decimals**: 2.92 / 3.10 / 2.88 / 3.08) vs JSON
`AverageAmplitude` 2.9 / 3.1 (JSON is rounded to one decimal — values differ by < 0.05), "Adaptive Amplitude" /
"Reduction" (label wraps, value on the first line) `AdaptiveAmplitudeReductionPercent`, "Time Adaptive Running"
`TimeAdaptiveRunningPercent`. Cells are framed by LW 0.05 rects (85 × 24 / 155 × 24). "END OF REPORT" (alpha 0 text)
at baseline 760 ends the document.

### 2.16 JSON content with no counterpart in the PDF

`ElectrodeFractionOf64`, the `Case` electrode state, `ProgramId`, `UpperCaptureAmplitudeInMilliAmps` /
`LowerCaptureAmplitudeInMilliAmps`, `BrainSensingStatus`, `SensingSetup.ChannelSignalResult`,
`MostRecentInSessionSignalCheck[]` (PSD), all of `DiagnosticData` (`LFPTrendLogs`, `LfpFrequencySnapshotEvents`,
`EventLogs`), `GroupHistory[].ActiveGroup` and `GroupHistory[].Groups[].GroupSettings`, history time-of-day,
`GroupUsagePercentage`, recharge sessions older than the last 10, `NonRechargeBatteryStatus`, device clock / counters,
survey reference electrodes. PDF content not in the JSON: "CLOSE COMMUNICATION REQUIRED FOR THERAPY OFF",
report print time, DEMO MODE, monthly group usage, 2-decimal average amplitude, per-cell impedance icons.

---------------------------------------------------------------------------------------------------------

## 3. Lead diagram geometry and the contact-mapping rule

### 3.1 Primitives (all four PDFs, identical)

| primitive | how it is drawn | size (pt) | where |
|---|---|---|---|
| contact circle | two paths with the same 4-Bézier circle: first `f` with white fill, then `S` black stroke **LW 0.4** | 10 × 10 (stimulation, history), 16 × 16 (lead configuration), 6 × 6 (annotations) | |
| lead shaft | filled black rect (`re f`), no stroke | 2 × 52 (stim/history), 2 × 73 (lead config), 2 × 30 (annotations) | vertical through the centre column |
| polarity glyph | one Type3 Roboto glyph per circle | see 3.3 | inside its circle |
| sensing icon | image XObject 28 × 23 px (all 80 placements decode to the same pixels, SHA-256 of data starts `4dc46146ff5a`, SMask `a3ee7ccba7b3`), drawn 5.88 × 4.83 pt | | inside a circle, at (circle.x0 + 2, circle.top + 2) |
| program box | stroked rect 221 wide, **LW 2.0 = active group, 0.3 = inactive** | 221 × 215 (A–C), 221 × 485 or 221 × 278 (D) | |
| slot separator | vertical line, same LW as its box | h 173 at x = box.x0 + 110, from box.top + 21 | only in boxes with 2 program slots (A–C) |
| empty-slot mark | two white `-` glyphs 6.7 pt, alpha 0.0039 (`--`) | | x0 = box.x0 + 165, baseline box.top + 6 (unused 2nd slot) |
| current separator | line LW 1, 65 wide (100 wide in group D) | | between the frequency line and the per-contact current lines |

Circle strokes are LW 0.4 for every contact; no line width, colour or fill change marks an active contact. The
IPG case is never drawn.

### 3.2 Coordinates of the stimulation diagrams

Shaft = filled rect with |w − 2| < 0.5 and |h − 52| < 0.6. Shaft top = box.top + 6. Shaft x0 relative to the box:

| diagram | shaft x0 | examples (Initial / Final column) |
|---|---|---|
| regular group, program 1 | box.x0 + 54 | 114 / 369 |
| regular group, program 2 (interleaved) | box.x0 + 164 | 224 / 479 |
| BrainSense group D (one program) | box.x0 + 44 | 104 / 359 |
| group history slot i (box 240 wide) | box.x0 + 60·i + 29 (BrainSense: + 21) | 104, 163, 223 / 373, 432, 492; D: 96, 215 / 365, 484 |
| annotation row | x0 = 99, top = date baseline + 5 | |

With `cx = shaft.x0 + 1` (shaft centre) and `t = shaft.top`, circles (10 pt, radius 5) are centred at

* rows: y = t + 13, t + 26, t + 39, t + 52 (pitch 13; the shaft ends at the bottom circle's centre and sticks out
  8 pt above the top circle);
* columns: x = cx (ring levels and the b segment), cx − 13 (segment a), cx + 13 (segment c).

Example, 539 page 3 Initial Group A left (box 60,140–281,355, LW 2.0): shaft 114,146–116,198; circles centred
(115, 159), (115, 172), (115, 185), (115, 198). 558 page 3 same box: 8 circles at (115,159); (102,172) (115,172)
(128,172); (102,185) (115,185) (128,185); (115,198).

Lead-configuration drawing (page 2): 16 pt circles, shaft 2 × 73 from box.top + 15; shaft centre x =
box.x0 + 53 (127, 247, 382, 502); row centres box.top + 33.45, + 51.9, + 70.35, + 88.8 (pitch 18.45); columns
cx − 18 / cx / cx + 18. The printed labels inside these circles ("3", "2a", "2b", "2c", "1a", "1b", "1c", "0" or
"11", "10a" …) confirm the row/column → label rule below.

Annotation drawing: 6 pt circles, row centres t + 7.5·k (k = 1…4), columns cx − 8 / cx / cx + 8.

### 3.3 Polarity glyphs

| meaning | glyph | font / size | fill | glyph origin relative to its circle | count (stim + history, 4 PDFs) |
|---|---|---|---|---|---|
| cathode (−) | `-` U+002D | Roboto 8.7 | **black** | x0 = circle.x0 + 4, baseline = circle.bottom − 2 | 142 + 330 |
| anode (+) | `+` U+002B | Roboto 6.7 | black | x0 = circle.x0 + 3, baseline = circle.bottom − 3 | 31 + 80 |
| off | `.` U+002E | Roboto 6.7 | **white** (invisible) | x0 = circle.x0 + 4, baseline = circle.bottom − 3 | 211 + 598 |
| sensing (BrainSense) | no glyph, eye image | | | image at circle.x0 + 2, circle.top + 2 | 48 + 72 |

Annotations: `-` 8.7 pt baseline = circle.bottom, `.` 6.7 pt white baseline = circle.bottom − 1.5, x0 = circle.x0 + 2.
Every circle contains exactly one glyph or one eye image (verified over all 1 472 circles = 1 512 contact
positions, collapsed sensing levels counted three times in the table above); a cathode is never also sensing. Assignment rule: glyph reference point (x0 + advance/2, baseline − 0.35·size) inside the circle bbox,
tolerance 1 pt (circles are ≥ 3 pt apart, so tolerance 1–1.5 pt is safe). Do not use pdfplumber `top` of the
glyphs for this test without the baseline correction of §1.3 (all glyphs are regular-weight, so `bottom` =
baseline). Polarity from the glyph character alone is sufficient (`.` never appears black inside a circle, `-`/`+`
never white); checking colour is a useful consistency check. Glyphs drawn by `-` in score columns (annotations)
and the empty-slot `--` are outside circles.

The xpdf text dump shows these glyphs as stray `.`, `-`, `+`, `--` characters scattered through the text; they
must be removed from text lines before parsing values (e.g. a white `.` shares the baseline of "60 µs" in group D).

### 3.4 Deterministic contact mapping

For each diagram (one shaft):

1. Collect stroked 10 × 10 circles with |centre.x − cx| ≤ 20 and t − 2 ≤ centre.y ≤ shaft.bottom + 8.
2. Group them into rows by centre y (tolerance 2 pt); there must be exactly 4 rows; sort rows top → bottom.
3. Lead of this diagram = lead configuration of the same snapshot (Initial/Final) and hemisphere (history and
   annotations: use the session's Final configuration). Let `s` = first contact number (0 or 8).
4. Ring lead (4 contacts): rows top→bottom = labels `s+3`, `s+2`, `s+1`, `s`; each row has 1 circle.
5. Directional lead (8 contacts): row 1 = `s+3` (1 circle), row 2 = level `s+2`, row 3 = level `s+1`, row 4 = `s`.
   A level row has either **3 circles** (sorted by x → suffixes a, b, c) or **1 circle** (only in BrainSense
   diagrams: the level is used as a whole ring for sensing; it carries the eye icon and expands to a, b, c with the
   same state). Any other count → error, do not guess.
6. Polarity per circle from §3.3. JSON index = label − s (`FourElectrodes_k`, `SenSight_k[a|b|c]`).
7. Case: not printed. Monopolar (no `+` contact) ⇔ JSON `Case` = `Positive`; at least one `+` contact ⇔ JSON
   `Case` = `None`. Verified on all 72 stimulation programs (55 + 17); history programs follow the same JSON
   pattern.
8. Sensing contacts (group D): the circles with the eye icon. They map to JSON `SensingChannel[h].Channel`
   `SensingElectrodeConfigDef.ZERO_AND_TWO` = levels {0, 2} (+ s) etc. (`ONE_AND_THREE`, `ZERO_AND_THREE`,
   `ONE_AND_TWO`, `ZERO_AND_ONE`, `TWO_AND_THREE` occur in survey data). Verified 16/16 programs.

### 3.5 Initial vs Final, hemispheres, program slots

* Column: diagram/box x0 < 306 → Initial (box x0 = 60), else Final (box x0 = 315). Lead-config boxes: 74 / 194
  Initial, 329 / 449 Final.
* Hemisphere: the `Left|Right <target>` line 10 pt above the box (baseline box.top − 10). Regular groups print
  Left box above Right box on one page (539/558 p3: tops 140 and 386; p4–5: 145 and 391); BrainSense group D uses
  one page per hemisphere (tops 145 and 129).
* Program number inside a hemisphere = order of shafts by x (program 1 left of the separator, program 2 right).
  JSON `Programs[0]` = `FIRST_LEFT/RIGHT`, `Programs[1]` = `SECOND_LEFT`. More than one program ⇒ interleaving
  (Medtronic "programs" share the group rate; the PDF has no explicit pulse-timing info).
* Text of a program = text lines whose centre lies in the program's slot: x between box.x0 and the separator
  (program 1) or between separator and box.x1 (program 2); whole box width if there is no separator
  (group D); vertically from shaft.top − 12 down to the "SENSING SETUP" heading (group D) or the box bottom.

### 3.6 Amplitude, pulse width, rate and per-contact currents

Regular program: amplitude / pulse width / rate lines at x0 = shaft.x0 − 12, baselines shaft.bottom + 16, + 29,
+ 42. If the program has ≥ 2 cathodes: current separator line (LW 1, 65 wide) at y = shaft.bottom + 46 from
x = shaft.x0 − 26, then one line per cathode at x0 = shaft.x0 − 26, baselines shaft.bottom + 58 + 13·i.
Group D: text column x0 = shaft.x0 + 32: "BrainSense: …" (8 pt) baseline t + 7, "Adaptive Therapy: …" t + 17,
amplitude t + 30, pulse width t + 43, rate t + 56, separator (100 wide) at t + 60, currents from t + 72 (pitch 13).
History: amplitude lines at x0 = shaft.x0 − 12 (D: − 6), baselines shaft.bottom + 16 / 29 / 42; never currents.

Current lines are ordered like the diagram (top level first, a → c), label = contact label of §3.4
(`2: 0.8 mA`, `10a: 1.0 mA`). Rules observed on all 72 programs:

* exactly the cathodes get a current line when there are ≥ 2 cathodes (39 programs); every printed value equals
  JSON `ElectrodeAmplitudeInMilliAmps` of that contact;
* a single-cathode program prints no current line (33 programs); JSON current of that cathode equals the program
  amplitude in all 33 cases (stimulation and BrainSense);
* anodes never get a current (JSON has only a positive `ElectrodeFractionOf64` for them);
* `ElectrodeFractionOf64` (−64 single cathode; e.g. −11/−21/−9 with +41 anode) is never printed.

### 3.7 Measured consistency issues (synthetic data, but parse defensively)

Programs whose printed/JSON cathode currents do not sum to the printed amplitude (|Σ − A| > 0.05 mA):

| PDF | snapshot / group / side / program | Σ cathode currents | amplitude | comment |
|---|---|---|---|---|
| 539 | Final A left 1 | 3.7 (0:1.0, 1:1.9, 2:0.8) | 5.7 | with anode on 3 |
| 539 | Final D right 1 | 5.8 (9:2.9, 10:2.9) | 2.9 | BrainSense, amplitude = paused amplitude |
| 548 | Final B left 2 | 3.4 | 2.3 | |
| 558, 606 | Initial A left; Initial A right; Final A right | 3.0 (3 × 1.0) | 2.9 / 3.1 / 3.1 | display rounding of the per-segment values |
| 558, 606 | Initial B right; 558 Final B right | 3.0 | 3.1 | rounding |
| 558, 606 | Initial+Final B left 1 | 1.6 (1a 1.0, 1c 0.6) | 2.9 | |
| 558, 606 | Initial+Final B left 2 | 1.2 (3 × 0.4) | 1.1 | rounding |
| 558, 606 | Initial C left/right, 558 Final C right, 606 Final C left/right | 1.2 (3 × 0.4) | 1.3 | rounding |
| 558 | Final C left 1 | 4.2 (2a 0.3, 2c 0.6, 1a 1.0, 1b 1.8, 1c 0.5) | 5.4 | |
| 558, 606 | Initial+Final D left | 1.6 (1a 1.0, 1b 0.6) | 2.9 | BrainSense |
| 558 / 606 | Initial+Final D right | 1.7 / 0.9 | 3.1 / 1.6 | BrainSense |
| 606 | Final A left 1 | 4.6 | 4.2 | |
| 606 | Final B right 1 | 4.3 | 3.1 | |

Also: 558 Final A left is a 0.0 mA program (cathodes 1a/1b/1c at 0.0 mA, anode 3) in an inactive group.
Counts: 39 multi-cathode programs; 38 of them have Σ ≠ amplitude by more than 0.05 mA (only the 0.0 mA program
agrees); 18 differ by more than the display-rounding tolerance 0.05·(n + 1) mA (n = number of cathodes).

The JSON contains exactly the same inconsistent numbers, so these are properties of the (synthetic) data, not
extraction errors: keep the printed program amplitude and the printed per-contact currents as separate fields,
never derive one from the other except for the single-cathode case.

### 3.8 Verification of the rule against the vendor JSON

Independent re-implementation (`scripts/ref_extract.py`, `compare.py`, `annot.py`, `imped.py`, `gdcheck.py`):

| check | 539 | 548 | 558 | 606 | total | mismatches |
|---|---|---|---|---|---|---|
| stimulation programs (contacts+polarity, printed currents, sensing levels, amplitude, PW, rate, active group, group name) | 18 | 18 | 18 | 18 | 72 | 0 |
| group-history programs (contacts+polarity, sensing, amplitude, PW, rate; date from orphan-aware rule) | 45 | 45 | 45 | 45 | 180 | 0 |
| annotations (hemisphere, date, amplitude, PW, rate, cathodes, all scores; multiset match) | 34 | 34 | 50 | 50 | 168 | 0 |
| impedance cells (hemisphere, contact pair, ohm) | 20 | 20 | 72 | 72 | 184 | 0 |
| BrainSense D settings (16 fields each) | 4 | 4 | 4 | 4 | 16 | 0 |

Amplitude source for the comparison: `AmplitudeInMilliAmps` for regular programs, `SuspendAmplitudeInMilliAmps`
for BrainSense `SensingChannel` entries (the only amplitude in them). Full per-program tables follow in §3.9 and
§3.10 (also as `ref/stim_table.md`, `ref/hist_table.md`).

### 3.9 Stimulation programs, PDF rule vs JSON (all 72)

Columns: snap I/F, active = thick border, contacts `label±` (off omitted), sensing = levels with eye icon,
amp/pw/rate printed; JSON case = state of `ElectrodeDef.Case` (+ Positive, off None); JSON amp source Amp =
`AmplitudeInMilliAmps`, Suspend = `SuspendAmplitudeInMilliAmps`. Unprinted single-cathode currents appear only in the
JSON column.

| PDF | snap | grp | side | prog | active | PDF contacts | PDF currents | PDF sensing | amp/pw/rate (PDF) | JSON contacts | JSON currents | JSON sensing | JSON case | JSON amp (src) | match |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 539 | I | A | L | 1 | Y | 1- |  |  | 2.9/60/130 | 1- | 1:2.9 |  | + | 2.9 (Amp) | OK |
| 539 | I | A | R | 1 | Y | 10- |  |  | 3.1/60/130 | 10- | 10:3.1 |  | + | 3.1 (Amp) | OK |
| 539 | I | B | L | 1 |  | 1- |  |  | 2.9/60/125 | 1- | 1:2.9 |  | + | 2.9 (Amp) | OK |
| 539 | I | B | L | 2 |  | 2- |  |  | 1.1/60/125 | 2- | 2:1.1 |  | + | 1.1 (Amp) | OK |
| 539 | I | B | R | 1 |  | 10- |  |  | 3.1/60/125 | 10- | 10:3.1 |  | + | 3.1 (Amp) | OK |
| 539 | I | C | L | 1 |  | 1-,2+ |  |  | 1.3/60/150 | 1-,2+ | 1:1.3 |  | off | 1.3 (Amp) | OK |
| 539 | I | C | R | 1 |  | 9-,10+ |  |  | 1.3/60/100 | 9-,10+ | 9:1.3 |  | off | 1.3 (Amp) | OK |
| 539 | I | D | L | 1 |  | 1- |  | [0, 2] | 2.9/60/125 | 1- | 1:2.9 | [0, 2] | + | 2.9 (Suspend) | OK |
| 539 | I | D | R | 1 |  | 10- |  | [9, 11] | 3.1/60/125 | 10- | 10:3.1 | [9, 11] | + | 3.1 (Suspend) | OK |
| 539 | F | A | L | 1 |  | 0-,1-,2-,3+ | 0:1,1:1.9,2:0.8 |  | 5.7/60/130 | 0-,1-,2-,3+ | 0:1,1:1.9,2:0.8 |  | off | 5.7 (Amp) | OK |
| 539 | F | A | R | 1 |  | 10- |  |  | 3.1/60/130 | 10- | 10:3.1 |  | + | 3.1 (Amp) | OK |
| 539 | F | B | L | 1 |  | 1- |  |  | 2.9/60/125 | 1- | 1:2.9 |  | + | 2.9 (Amp) | OK |
| 539 | F | B | L | 2 |  | 2- |  |  | 1.1/60/125 | 2- | 2:1.1 |  | + | 1.1 (Amp) | OK |
| 539 | F | B | R | 1 |  | 10- |  |  | 3.1/60/125 | 10- | 10:3.1 |  | + | 3.1 (Amp) | OK |
| 539 | F | C | L | 1 |  | 1-,2+ |  |  | 1.3/60/150 | 1-,2+ | 1:1.3 |  | off | 1.3 (Amp) | OK |
| 539 | F | C | R | 1 |  | 9-,10+ |  |  | 1.3/60/100 | 9-,10+ | 9:1.3 |  | off | 1.3 (Amp) | OK |
| 539 | F | D | L | 1 | Y | 1- |  | [0, 2] | 2.9/60/125 | 1- | 1:2.9 | [0, 2] | + | 2.9 (Suspend) | OK |
| 539 | F | D | R | 1 | Y | 9-,10- | 9:2.9,10:2.9 | [8, 11] | 2.9/60/125 | 9-,10- | 9:2.9,10:2.9 | [8, 11] | + | 2.9 (Suspend) | OK |
| 548 | I | A | L | 1 | Y | 1- |  |  | 2.9/60/130 | 1- | 1:2.9 |  | + | 2.9 (Amp) | OK |
| 548 | I | A | R | 1 | Y | 10- |  |  | 3.1/60/130 | 10- | 10:3.1 |  | + | 3.1 (Amp) | OK |
| 548 | I | B | L | 1 |  | 1- |  |  | 2.9/60/125 | 1- | 1:2.9 |  | + | 2.9 (Amp) | OK |
| 548 | I | B | L | 2 |  | 2- |  |  | 1.1/60/125 | 2- | 2:1.1 |  | + | 1.1 (Amp) | OK |
| 548 | I | B | R | 1 |  | 10- |  |  | 3.1/60/125 | 10- | 10:3.1 |  | + | 3.1 (Amp) | OK |
| 548 | I | C | L | 1 |  | 1-,2+ |  |  | 1.3/60/150 | 1-,2+ | 1:1.3 |  | off | 1.3 (Amp) | OK |
| 548 | I | C | R | 1 |  | 9-,10+ |  |  | 1.3/60/100 | 9-,10+ | 9:1.3 |  | off | 1.3 (Amp) | OK |
| 548 | I | D | L | 1 |  | 1- |  | [0, 2] | 2.9/60/125 | 1- | 1:2.9 | [0, 2] | + | 2.9 (Suspend) | OK |
| 548 | I | D | R | 1 |  | 10- |  | [9, 11] | 3.1/60/125 | 10- | 10:3.1 | [9, 11] | + | 3.1 (Suspend) | OK |
| 548 | F | A | L | 1 |  | 1- |  |  | 2.9/60/130 | 1- | 1:2.9 |  | + | 2.9 (Amp) | OK |
| 548 | F | A | R | 1 |  | 10- |  |  | 2.2/60/130 | 10- | 10:2.2 |  | + | 2.2 (Amp) | OK |
| 548 | F | B | L | 1 |  | 1- |  |  | 2.9/60/125 | 1- | 1:2.9 |  | + | 2.9 (Amp) | OK |
| 548 | F | B | L | 2 |  | 0-,2- | 0:2.3,2:1.1 |  | 2.3/60/125 | 0-,2- | 0:2.3,2:1.1 |  | + | 2.3 (Amp) | OK |
| 548 | F | B | R | 1 |  | 10- |  |  | 2/60/125 | 10- | 10:2 |  | + | 2 (Amp) | OK |
| 548 | F | C | L | 1 |  | 1-,2+ |  |  | 1.3/60/150 | 1-,2+ | 1:1.3 |  | off | 1.3 (Amp) | OK |
| 548 | F | C | R | 1 |  | 9-,10+ |  |  | 1.3/60/100 | 9-,10+ | 9:1.3 |  | off | 1.3 (Amp) | OK |
| 548 | F | D | L | 1 | Y | 1- |  | [0, 2] | 2.9/60/125 | 1- | 1:2.9 | [0, 2] | + | 2.9 (Suspend) | OK |
| 548 | F | D | R | 1 | Y | 10- |  | [9, 11] | 3.1/60/125 | 10- | 10:3.1 | [9, 11] | + | 3.1 (Suspend) | OK |
| 558 | I | A | L | 1 | Y | 1a-,1b-,1c- | 1a:1,1b:1,1c:1 |  | 2.9/60/130 | 1a-,1b-,1c- | 1a:1,1b:1,1c:1 |  | + | 2.9 (Amp) | OK |
| 558 | I | A | R | 1 | Y | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | 3.1/60/130 | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | + | 3.1 (Amp) | OK |
| 558 | I | B | L | 1 |  | 1a-,1c- | 1a:1,1c:0.6 |  | 2.9/60/125 | 1a-,1c- | 1a:1,1c:0.6 |  | + | 2.9 (Amp) | OK |
| 558 | I | B | L | 2 |  | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | 1.1/60/125 | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | + | 1.1 (Amp) | OK |
| 558 | I | B | R | 1 |  | 10a-,10b-,10c- | 10a:1,10b:1,10c:1 |  | 3.1/60/125 | 10a-,10b-,10c- | 10a:1,10b:1,10c:1 |  | + | 3.1 (Amp) | OK |
| 558 | I | C | L | 1 |  | 1a-,1b-,1c-,2a+,2b+,2c+ | 1a:0.4,1b:0.4,1c:0.4 |  | 1.3/60/150 | 1a-,1b-,1c-,2a+,2b+,2c+ | 1a:0.4,1b:0.4,1c:0.4 |  | off | 1.3 (Amp) | OK |
| 558 | I | C | R | 1 |  | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | 1.3/60/100 | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | off | 1.3 (Amp) | OK |
| 558 | I | D | L | 1 |  | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | 2.9/60/125 | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | + | 2.9 (Suspend) | OK |
| 558 | I | D | R | 1 |  | 10a-,10c- | 10a:1,10c:0.7 | [9, 11] | 3.1/60/125 | 10a-,10c- | 10a:1,10c:0.7 | [9, 11] | + | 3.1 (Suspend) | OK |
| 558 | F | A | L | 1 |  | 1a-,1b-,1c-,3+ | 1a:0,1b:0,1c:0 |  | 0/60/130 | 1a-,1b-,1c-,3+ | 1a:0,1b:0,1c:0 |  | off | 0 (Amp) | OK |
| 558 | F | A | R | 1 |  | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | 3.1/60/130 | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | + | 3.1 (Amp) | OK |
| 558 | F | B | L | 1 |  | 1a-,1c- | 1a:1,1c:0.6 |  | 2.9/60/125 | 1a-,1c- | 1a:1,1c:0.6 |  | + | 2.9 (Amp) | OK |
| 558 | F | B | L | 2 |  | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | 1.1/60/125 | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | + | 1.1 (Amp) | OK |
| 558 | F | B | R | 1 |  | 10a-,10b-,10c- | 10a:1,10b:1,10c:1 |  | 3.1/60/125 | 10a-,10b-,10c- | 10a:1,10b:1,10c:1 |  | + | 3.1 (Amp) | OK |
| 558 | F | C | L | 1 | Y | 1a-,1b-,1c-,2a-,2c- | 1a:1,1b:1.8,1c:0.5,2a:0.3,2c:0.6 |  | 5.4/60/150 | 1a-,1b-,1c-,2a-,2c- | 1a:1,1b:1.8,1c:0.5,2a:0.3,2c:0.6 |  | + | 5.4 (Amp) | OK |
| 558 | F | C | R | 1 | Y | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | 1.3/60/100 | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | off | 1.3 (Amp) | OK |
| 558 | F | D | L | 1 |  | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | 2.9/60/125 | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | + | 2.9 (Suspend) | OK |
| 558 | F | D | R | 1 |  | 10a-,10c- | 10a:1,10c:0.7 | [9, 11] | 3.1/60/125 | 10a-,10c- | 10a:1,10c:0.7 | [9, 11] | + | 3.1 (Suspend) | OK |
| 606 | I | A | L | 1 | Y | 1a-,1b-,1c- | 1a:1,1b:1,1c:1 |  | 2.9/60/130 | 1a-,1b-,1c- | 1a:1,1b:1,1c:1 |  | + | 2.9 (Amp) | OK |
| 606 | I | A | R | 1 | Y | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | 3.1/60/130 | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | + | 3.1 (Amp) | OK |
| 606 | I | B | L | 1 |  | 1a-,1c- | 1a:1,1c:0.6 |  | 2.9/60/125 | 1a-,1c- | 1a:1,1c:0.6 |  | + | 2.9 (Amp) | OK |
| 606 | I | B | L | 2 |  | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | 1.1/60/125 | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | + | 1.1 (Amp) | OK |
| 606 | I | B | R | 1 |  | 10a-,10b-,10c- | 10a:1,10b:1,10c:1 |  | 3.1/60/125 | 10a-,10b-,10c- | 10a:1,10b:1,10c:1 |  | + | 3.1 (Amp) | OK |
| 606 | I | C | L | 1 |  | 1a-,1b-,1c-,2a+,2b+,2c+ | 1a:0.4,1b:0.4,1c:0.4 |  | 1.3/60/150 | 1a-,1b-,1c-,2a+,2b+,2c+ | 1a:0.4,1b:0.4,1c:0.4 |  | off | 1.3 (Amp) | OK |
| 606 | I | C | R | 1 |  | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | 1.3/60/100 | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | off | 1.3 (Amp) | OK |
| 606 | I | D | L | 1 |  | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | 2.9/60/125 | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | + | 2.9 (Suspend) | OK |
| 606 | I | D | R | 1 |  | 10a-,10c- | 10a:0.5,10c:0.4 | [9, 11] | 1.6/60/125 | 10a-,10c- | 10a:0.5,10c:0.4 | [9, 11] | + | 1.6 (Suspend) | OK |
| 606 | F | A | L | 1 |  | 1a-,1b-,1c-,2a-,2b-,2c- | 1a:1.4,1b:0.4,1c:0.7,2a:0.7,2b:0.7,2c:0.7 |  | 4.2/60/130 | 1a-,1b-,1c-,2a-,2b-,2c- | 1a:1.4,1b:0.4,1c:0.7,2a:0.7,2b:0.7,2c:0.7 |  | + | 4.2 (Amp) | OK |
| 606 | F | A | R | 1 |  | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | 3.1/60/130 | 9a-,9b-,9c- | 9a:1,9b:1,9c:1 |  | + | 3.1 (Amp) | OK |
| 606 | F | B | L | 1 | Y | 1a-,1c- | 1a:1,1c:0.6 |  | 2.9/60/125 | 1a-,1c- | 1a:1,1c:0.6 |  | + | 2.9 (Amp) | OK |
| 606 | F | B | L | 2 | Y | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | 1.1/60/125 | 2a-,2b-,2c- | 2a:0.4,2b:0.4,2c:0.4 |  | + | 1.1 (Amp) | OK |
| 606 | F | B | R | 1 | Y | 8-,10a-,10b-,10c- | 8:1.3,10a:1,10b:1,10c:1 |  | 3.1/60/125 | 8-,10a-,10b-,10c- | 8:1.3,10a:1,10b:1,10c:1 |  | + | 3.1 (Amp) | OK |
| 606 | F | C | L | 1 |  | 1a-,1b-,1c-,2a+,2b+,2c+ | 1a:0.4,1b:0.4,1c:0.4 |  | 1.3/60/150 | 1a-,1b-,1c-,2a+,2b+,2c+ | 1a:0.4,1b:0.4,1c:0.4 |  | off | 1.3 (Amp) | OK |
| 606 | F | C | R | 1 |  | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | 1.3/60/100 | 9a+,9b+,9c+,10a-,10b-,10c- | 10a:0.4,10b:0.4,10c:0.4 |  | off | 1.3 (Amp) | OK |
| 606 | F | D | L | 1 |  | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | 2.9/60/125 | 1a-,1b- | 1a:1,1b:0.6 | [0, 2] | + | 2.9 (Suspend) | OK |
| 606 | F | D | R | 1 |  | 10a-,10c- | 10a:0.5,10c:0.4 | [9, 11] | 1.6/60/125 | 10a-,10c- | 10a:0.5,10c:0.4 | [9, 11] | + | 1.6 (Suspend) | OK |

### 3.10 Group-history programs, PDF rule vs JSON (all 180, one row per session date and group)

Format: `contacts [S sensing levels] amp/pw/rate`, programs of one hemisphere joined with `+`; `=` means the JSON value is identical. History contact labels use the session lead configuration.

| PDF | date (PDF) | JSON SessionDate (UTC) | grp | Left: PDF / JSON | Right: PDF / JSON | match |
|---|---|---|---|---|---|---|
| 539 | Sep 30, 2026 | 2026-09-30T06:51:41Z | A | 1- 2.9/60/130 / = | 10- 3.1/60/130 / = | OK |
| 539 | Sep 30, 2026 | 2026-09-30T06:51:41Z | B | 1- 2.9/60/125 + 2- 1.1/60/125 / = | 10- 3.1/60/125 / = | OK |
| 539 | Sep 30, 2026 | 2026-09-30T06:51:41Z | C | 1-,2+ 1.3/60/150 / = | 9-,10+ 1.3/60/100 / = | OK |
| 539 | Sep 30, 2026 | 2026-09-30T06:51:41Z | D | 1- S0,2 2.9/60/125 / = | 10- S9,11 3.1/60/125 / = | OK |
| 539 | Jul 2, 2026 | 2026-07-02T06:51:41Z | A | 1- 2.9/60/130 / = | 10- 3.1/60/130 / = | OK |
| 539 | Jul 2, 2026 | 2026-07-02T06:51:41Z | B | 1- 2.9/60/125 + 2- 1.1/60/125 / = | 10- 3.1/60/125 / = | OK |
| 539 | Jul 2, 2026 | 2026-07-02T06:51:41Z | C | 1-,2+ 1.3/60/150 / = | 9-,10+ 1.3/60/100 / = | OK |
| 539 | Jul 2, 2026 | 2026-07-02T06:51:41Z | D | 1- S0,2 2.9/60/125 / = | 10- S9,11 3.1/60/125 / = | OK |
| 539 | Jun 2, 2026 | 2026-06-02T06:51:41Z | A | 1- 2.8/60/130 / = | 10- 3/60/130 / = | OK |
| 539 | Jun 2, 2026 | 2026-06-02T06:51:41Z | B | 1- 2.8/60/125 + 2- 1/60/125 / = | 10- 3/60/125 / = | OK |
| 539 | Jun 2, 2026 | 2026-06-02T06:51:41Z | C | 1-,2+ 1.2/60/150 / = | 9-,10+ 1.2/60/100 / = | OK |
| 539 | Jun 2, 2026 | 2026-06-02T06:51:41Z | D | 1- 2.8/60/125 / = | 10- 3/60/125 / = | OK |
| 539 | May 3, 2026 | 2026-05-03T06:51:41Z | A | 1- 2.6/60/130 / = | 10- 2.9/60/130 / = | OK |
| 539 | May 3, 2026 | 2026-05-03T06:51:41Z | B | 1- 2.6/60/125 + 2- 0.9/60/125 / = | 10- 2.9/60/125 / = | OK |
| 539 | May 3, 2026 | 2026-05-03T06:51:41Z | C | 1-,2+ 1.1/60/150 / = | 9-,10+ 1.1/60/100 / = | OK |
| 539 | May 3, 2026 | 2026-05-03T06:51:41Z | D | 1- S0,2 2.6/60/125 / = | 10- S9,11 2.9/60/125 / = | OK |
| 539 | Apr 3, 2026 | 2026-04-03T06:51:41Z | A | 1- 2.6/60/130 / = | 10- 2.8/60/130 / = | OK |
| 539 | Apr 3, 2026 | 2026-04-03T06:51:41Z | B | 1- 2.6/60/125 + 2- 0.8/60/125 / = | 10- 2.8/60/125 / = | OK |
| 539 | Apr 3, 2026 | 2026-04-03T06:51:41Z | C | 1-,2+ 1/60/150 / = | 9-,10+ 1/60/100 / = | OK |
| 539 | Apr 3, 2026 | 2026-04-03T06:51:41Z | D | 1- 2.6/60/125 / = | 10- 2.8/60/125 / = | OK |
| 548 | Sep 30, 2026 | 2026-09-30T06:57:49Z | A | 1- 2.9/60/130 / = | 10- 3.1/60/130 / = | OK |
| 548 | Sep 30, 2026 | 2026-09-30T06:57:49Z | B | 1- 2.9/60/125 + 2- 1.1/60/125 / = | 10- 3.1/60/125 / = | OK |
| 548 | Sep 30, 2026 | 2026-09-30T06:57:49Z | C | 1-,2+ 1.3/60/150 / = | 9-,10+ 1.3/60/100 / = | OK |
| 548 | Sep 30, 2026 | 2026-09-30T06:57:49Z | D | 1- S0,2 2.9/60/125 / = | 10- S9,11 3.1/60/125 / = | OK |
| 548 | Jul 2, 2026 | 2026-07-02T06:57:49Z | A | 1- 2.9/60/130 / = | 10- 3.1/60/130 / = | OK |
| 548 | Jul 2, 2026 | 2026-07-02T06:57:49Z | B | 1- 2.9/60/125 + 2- 1.1/60/125 / = | 10- 3.1/60/125 / = | OK |
| 548 | Jul 2, 2026 | 2026-07-02T06:57:49Z | C | 1-,2+ 1.3/60/150 / = | 9-,10+ 1.3/60/100 / = | OK |
| 548 | Jul 2, 2026 | 2026-07-02T06:57:49Z | D | 1- S0,2 2.9/60/125 / = | 10- S9,11 3.1/60/125 / = | OK |
| 548 | Jun 2, 2026 | 2026-06-02T06:57:49Z | A | 1- 2.8/60/130 / = | 10- 3/60/130 / = | OK |
| 548 | Jun 2, 2026 | 2026-06-02T06:57:49Z | B | 1- 2.8/60/125 + 2- 1/60/125 / = | 10- 3/60/125 / = | OK |
| 548 | Jun 2, 2026 | 2026-06-02T06:57:49Z | C | 1-,2+ 1.2/60/150 / = | 9-,10+ 1.2/60/100 / = | OK |
| 548 | Jun 2, 2026 | 2026-06-02T06:57:49Z | D | 1- 2.8/60/125 / = | 10- 3/60/125 / = | OK |
| 548 | May 3, 2026 | 2026-05-03T06:57:49Z | A | 1- 2.6/60/130 / = | 10- 2.9/60/130 / = | OK |
| 548 | May 3, 2026 | 2026-05-03T06:57:49Z | B | 1- 2.6/60/125 + 2- 0.9/60/125 / = | 10- 2.9/60/125 / = | OK |
| 548 | May 3, 2026 | 2026-05-03T06:57:49Z | C | 1-,2+ 1.1/60/150 / = | 9-,10+ 1.1/60/100 / = | OK |
| 548 | May 3, 2026 | 2026-05-03T06:57:49Z | D | 1- S0,2 2.6/60/125 / = | 10- S9,11 2.9/60/125 / = | OK |
| 548 | Apr 3, 2026 | 2026-04-03T06:57:49Z | A | 1- 2.6/60/130 / = | 10- 2.8/60/130 / = | OK |
| 548 | Apr 3, 2026 | 2026-04-03T06:57:49Z | B | 1- 2.6/60/125 + 2- 0.8/60/125 / = | 10- 2.8/60/125 / = | OK |
| 548 | Apr 3, 2026 | 2026-04-03T06:57:49Z | C | 1-,2+ 1/60/150 / = | 9-,10+ 1/60/100 / = | OK |
| 548 | Apr 3, 2026 | 2026-04-03T06:57:49Z | D | 1- 2.6/60/125 / = | 10- 2.8/60/125 / = | OK |
| 558 | Sep 30, 2026 | 2026-09-30T06:59:41Z | A | 1a-,1b-,1c- 2.9/60/130 / = | 9a-,9b-,9c- 3.1/60/130 / = | OK |
| 558 | Sep 30, 2026 | 2026-09-30T06:59:41Z | B | 1a-,1c- 2.9/60/125 + 2a-,2b-,2c- 1.1/60/125 / = | 10a-,10b-,10c- 3.1/60/125 / = | OK |
| 558 | Sep 30, 2026 | 2026-09-30T06:59:41Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.3/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.3/60/100 / = | OK |
| 558 | Sep 30, 2026 | 2026-09-30T06:59:41Z | D | 1a-,1b- S0,2 2.9/60/125 / = | 10a-,10c- S9,11 3.1/60/125 / = | OK |
| 558 | Jul 2, 2026 | 2026-07-02T06:59:41Z | A | 1a-,1b-,1c- 2.9/60/130 / = | 9a-,9b-,9c- 3.1/60/130 / = | OK |
| 558 | Jul 2, 2026 | 2026-07-02T06:59:41Z | B | 1a-,1c- 2.9/60/125 + 2a-,2b-,2c- 1.1/60/125 / = | 10a-,10b-,10c- 3.1/60/125 / = | OK |
| 558 | Jul 2, 2026 | 2026-07-02T06:59:41Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.3/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.3/60/100 / = | OK |
| 558 | Jul 2, 2026 | 2026-07-02T06:59:41Z | D | 1a-,1b- S0,2 2.9/60/125 / = | 10a-,10c- S9,11 3.1/60/125 / = | OK |
| 558 | Jun 2, 2026 | 2026-06-02T06:59:41Z | A | 1a-,1b-,1c- 2.8/60/130 / = | 9a-,9b-,9c- 3/60/130 / = | OK |
| 558 | Jun 2, 2026 | 2026-06-02T06:59:41Z | B | 1a-,1c- 2.8/60/125 + 2a-,2b-,2c- 1/60/125 / = | 10a-,10b-,10c- 3/60/125 / = | OK |
| 558 | Jun 2, 2026 | 2026-06-02T06:59:41Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.2/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.2/60/100 / = | OK |
| 558 | Jun 2, 2026 | 2026-06-02T06:59:41Z | D | 1a-,1b- 2.8/60/125 / = | 10a-,10c- 3/60/125 / = | OK |
| 558 | May 3, 2026 | 2026-05-03T06:59:41Z | A | 1a-,1b-,1c- 2.6/60/130 / = | 9a-,9b-,9c- 2.9/60/130 / = | OK |
| 558 | May 3, 2026 | 2026-05-03T06:59:41Z | B | 1a-,1c- 2.6/60/125 + 2a-,2b-,2c- 0.9/60/125 / = | 10a-,10b-,10c- 2.9/60/125 / = | OK |
| 558 | May 3, 2026 | 2026-05-03T06:59:41Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.1/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.1/60/100 / = | OK |
| 558 | May 3, 2026 | 2026-05-03T06:59:41Z | D | 1a-,1b- S0,2 2.6/60/125 / = | 10a-,10c- S9,11 2.9/60/125 / = | OK |
| 558 | Apr 3, 2026 | 2026-04-03T06:59:41Z | A | 1a-,1b-,1c- 2.6/60/130 / = | 9a-,9b-,9c- 2.8/60/130 / = | OK |
| 558 | Apr 3, 2026 | 2026-04-03T06:59:41Z | B | 1a-,1c- 2.6/60/125 + 2a-,2b-,2c- 0.8/60/125 / = | 10a-,10b-,10c- 2.8/60/125 / = | OK |
| 558 | Apr 3, 2026 | 2026-04-03T06:59:41Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1/60/100 / = | OK |
| 558 | Apr 3, 2026 | 2026-04-03T06:59:41Z | D | 1a-,1b- 2.6/60/125 / = | 10a-,10c- 2.8/60/125 / = | OK |
| 606 | Sep 30, 2026 | 2026-09-30T07:01:37Z | A | 1a-,1b-,1c- 2.9/60/130 / = | 9a-,9b-,9c- 3.1/60/130 / = | OK |
| 606 | Sep 30, 2026 | 2026-09-30T07:01:37Z | B | 1a-,1c- 2.9/60/125 + 2a-,2b-,2c- 1.1/60/125 / = | 10a-,10b-,10c- 3.1/60/125 / = | OK |
| 606 | Sep 30, 2026 | 2026-09-30T07:01:37Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.3/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.3/60/100 / = | OK |
| 606 | Sep 30, 2026 | 2026-09-30T07:01:37Z | D | 1a-,1b- S0,2 2.9/60/125 / = | 10a-,10c- S9,11 1.6/60/125 / = | OK |
| 606 | Jul 2, 2026 | 2026-07-02T07:01:37Z | A | 1a-,1b-,1c- 2.9/60/130 / = | 9a-,9b-,9c- 3.1/60/130 / = | OK |
| 606 | Jul 2, 2026 | 2026-07-02T07:01:37Z | B | 1a-,1c- 2.9/60/125 + 2a-,2b-,2c- 1.1/60/125 / = | 10a-,10b-,10c- 3.1/60/125 / = | OK |
| 606 | Jul 2, 2026 | 2026-07-02T07:01:37Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.3/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.3/60/100 / = | OK |
| 606 | Jul 2, 2026 | 2026-07-02T07:01:37Z | D | 1a-,1b- S0,2 2.9/60/125 / = | 10a-,10c- S9,11 1.6/60/125 / = | OK |
| 606 | Jun 2, 2026 | 2026-06-02T07:01:37Z | A | 1a-,1b-,1c- 2.8/60/130 / = | 9a-,9b-,9c- 3/60/130 / = | OK |
| 606 | Jun 2, 2026 | 2026-06-02T07:01:37Z | B | 1a-,1c- 2.8/60/125 + 2a-,2b-,2c- 1/60/125 / = | 10a-,10b-,10c- 3/60/125 / = | OK |
| 606 | Jun 2, 2026 | 2026-06-02T07:01:37Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.2/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.2/60/100 / = | OK |
| 606 | Jun 2, 2026 | 2026-06-02T07:01:37Z | D | 1a-,1b- 2.8/60/125 / = | 10a-,10c- 1.5/60/125 / = | OK |
| 606 | May 3, 2026 | 2026-05-03T07:01:37Z | A | 1a-,1b-,1c- 2.6/60/130 / = | 9a-,9b-,9c- 2.9/60/130 / = | OK |
| 606 | May 3, 2026 | 2026-05-03T07:01:37Z | B | 1a-,1c- 2.6/60/125 + 2a-,2b-,2c- 0.9/60/125 / = | 10a-,10b-,10c- 2.9/60/125 / = | OK |
| 606 | May 3, 2026 | 2026-05-03T07:01:37Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1.1/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1.1/60/100 / = | OK |
| 606 | May 3, 2026 | 2026-05-03T07:01:37Z | D | 1a-,1b- S0,2 2.6/60/125 / = | 10a-,10c- S9,11 1.4/60/125 / = | OK |
| 606 | Apr 3, 2026 | 2026-04-03T07:01:37Z | A | 1a-,1b-,1c- 2.6/60/130 / = | 9a-,9b-,9c- 2.8/60/130 / = | OK |
| 606 | Apr 3, 2026 | 2026-04-03T07:01:37Z | B | 1a-,1c- 2.6/60/125 + 2a-,2b-,2c- 0.8/60/125 / = | 10a-,10b-,10c- 2.8/60/125 / = | OK |
| 606 | Apr 3, 2026 | 2026-04-03T07:01:37Z | C | 1a-,1b-,1c-,2a+,2b+,2c+ 1/60/150 / = | 9a+,9b+,9c+,10a-,10b-,10c- 1/60/100 / = | OK |
| 606 | Apr 3, 2026 | 2026-04-03T07:01:37Z | D | 1a-,1b- 2.6/60/125 / = | 10a-,10c- 1.3/60/125 / = | OK |
---------------------------------------------------------------------------------------------------------

## 4. Differences between the four PDFs

| | 539 | 548 | 558 | 606 |
|---|---|---|---|---|
| device | Percept PC B35200 | Percept RC B35300 | Percept PC B35200 | Percept RC B35300 |
| serial | NPI_FOLLOWUP_PERCEPT_PC | NND_FOLLOWUP_NGRC | NPI_FOLLOWUP_PERCEPT_PC | NND_FOLLOWUP_NGRC |
| lead | 3389 ring ×2 (0–3, 8–11) | 3389 ×2 | B33005 directional ×2 (0–7, 8–15, orientation 120°/240°) | B33005 ×2 |
| pages | 25 | 25 | 27 | 28 |
| device page | "Estimated Battery Remaining (based on last 7 days) 4 years, 1 month"; columns x 115/270/450 | "ERI Date Feb 28, 2041"; columns x 120/275/455 | as 539 | as 548 |
| battery recharge section `rc` | – | p17 | – | p18 |
| impedance | 1 page, both sides | 1 page | 2 pages (left p12, right p13, `<s-sii>` repeated) | 2 pages |
| annotations | 4 pages, 34 rows (rings only) | 4 pages, 34 rows | 6 pages, 50 rows (rings + single segments) | 6 pages, 50 rows |
| electrode survey / identifier | data (right STN, 6 bipolar pairs; identifier ranks) | "No Data Available" | "No Data Available" | "No Data Available" |
| active group Initial / Final (thick border = JSON `ActiveGroup`) | A / D | A / D | A / C | A / B |
| BrainSense status (group D) | Initial Adapting; Final left "Sensing Only", right Adapting | Initial Adapting; Final "Paused" (`SUSPENDED`) | Adapting | Adapting |
| JSON extras | LFPMontage, BrainSenseSurveys | RechargeCount, NonRechargeBatteryStatus | – | RechargeCount, NonRechargeBatteryStatus |

All four share the same section order, header/footer, fonts, box geometry and 4 groups A (Walking), B (Talking,
two programs on the left), C (MRI, bipolar with anode), D (BrainSense). No voltage-mode program, no unilateral lead,
no cycling, no group without stimulation and no B33015 lead occur — those paths are untested.

## 5. Edge cases and recommended handling

1. **Per-contact currents that do not sum to the amplitude** (§3.7, 38/39 multi-cathode programs, 18 beyond
   rounding): store `program_amplitude` and `contact_currents[]` independently; flag "sum mismatch" with the
   difference; never rescale.
2. **Unprinted currents**: single cathode → current = program amplitude (holds 33/33); multiple cathodes are always
   all printed; anode currents and `ElectrodeFractionOf64` are never printed — leave them null. Do not invent
   fractions from rounded currents.
3. **Case/return electrode**: not drawn. Derive "case is anode" iff no contact has `+` (252/252 incl. history).
   With mixed returns (contact anode + case) the PDF could not distinguish; in the samples every program with a
   contact anode has `Case` = None.
4. **Interleaving / second program**: 2 shafts in one hemisphere box (or slots 1/3 in history). Program order
   by x. Rates are group-level (or per program in group C); the PDF has no pulse-timing schedule.
5. **Collapsed sensing level** (directional, BrainSense only): one circle with eye icon on a level row → all three
   segments of that level, sensing, off. A collapsed row without an eye icon has not been seen → treat as error.
6. **Group D amplitude** = paused/suspend amplitude (`SuspendAmplitudeInMilliAmps`), also when "Adapting"; the
   adaptive range is in "Minimum/Maximum Limit". Keep `BrainSense mode` ("Adaptive" / "Sensing Only"),
   `adaptive status` ("Adapting" / "Paused"; absent for Sensing Only).
7. **Groups with stimulation off / therapy off**: "STIMULATION Off" would only change the header row; group
   boxes are still drawn. A 0.0 mA program exists (558 Final A left) — keep amplitude 0, polarity as drawn.
8. **Missing optional sections**: `rc` only for RC; survey/identifier may be "No Data Available"; some sections
   could be absent entirely in other configurations — parse by section token and tolerate absence.
9. **Continuation pages**: group D right box on the next page without a group heading; impedance right side on
   the next page; history dates orphaned at a page bottom; annotations rows continue with the hemisphere heading
   only at block start. Carry state (column group, column date, hemisphere) across pages.
10. **Line widths**: active-group detection needs ExtGState `/LW` tracking (pdfplumber/pdfminer report 0).
11. **Invisible text**: filter white glyphs (markers, `.`, `--`) from text; do *not* filter alpha-0 text (headers,
    "END OF REPORT", identifier labels) because that is the only text copy of vector-drawn words.
12. **Unicode**: µ (U+00B5) vs μ (U+03BC) both used; NBSP in bold; en dash in ranges; degree sign. Normalise
    before matching; accept both micro signs.
13. **Number formats**: one decimal for program amplitudes and currents, two decimals for annotation amplitudes
    and LFP frequencies, nine decimals for survey peak magnitudes, comma thousands separators in impedances,
    "Limit:3.3 mA" without space, "Value : 2.9 mA" with space before the colon, "Additional Behavior:LFP".
14. **Device IDs**: "0000" in the PDF vs "00.00" in JSON (application IDs); "--" = empty device name.
15. **Name split**: the PDF prints "First Last" only; header line 2 concatenates name and patient ID with a space.
    Keep the full display name; do not split (JSON has separate fields).
16. **Group usage / recharge / survey plots**: chart values are not reliably recoverable (bars are vector rects
    for usage and recharge, PSD curves for surveys, plus a full-page raster on usage/recharge pages); extract only the
    printed labels.
17. **Annotation "0" vs Rating_None**: a printed 0 maps to JSON `Rating_None`; "-" means not rated.
18. **Average amplitude** in the event summary has 2 decimals in the PDF but 1 in JSON — compare with 0.05 tolerance.

## 6. Dates, times and time zones

| where | printed format | example | JSON counterpart | conversion |
|---|---|---|---|---|
| header session date | `MMM d, yyyy h:mm a` (12 h, no leading zero) | "Sep 30, 2026 8:51 AM" | `SessionDate` "2026-09-30T06:51:45Z" | UTC → `ProgrammerTimezoneId` (Europe/Zurich), **truncate** seconds |
| footer print time | same | "Sep 30, 2026 9:05 AM" | – (file name `Report_SessionReport_20260930T090539` = local generation time) | |
| date of birth | `MMM d, yyyy` | "Jun 25, 1959" | `PatientDateOfBirth` "1959-06-24T23:00:00Z" | local midnight in Europe/Zurich (CET +01:00 in 1959) |
| implanted | `MMM d, yyyy` | "Mar 4, 2026" | `ImplantDate` "2026-03-04T07:51:41Z" | local date |
| ERI date (RC) | `MMM d, yyyy` | "Feb 28, 2041" | `BatteryInformation.ERIDate` | local date |
| check battery reminder | `HH:mm` 24 h | "11:00" | `BatteryReminder.ReminderTime` | none |
| history session | `MMM d, yyyy` | "Jul 2, 2026" | `GroupHistory[n].SessionDate` "2026-07-02T06:51:41Z" | local date |
| annotation | `MMM d, yyyy` | "Mar 30, 2026" | `Annotations[i].Date` "2026-03-29T22:00:00Z" | local midnight CEST (+02:00, DST began 2026-03-29 01:00 UTC); a naive UTC date would give Mar 29 |
| event summary range | `MMM dd – MMM dd` (zero-padded, U+2013, no year) | "Jul 02 – Sep 30" | `EventSummary.SessionStartDate` "2026-07-01T23:40:00Z" / `SessionEndDate` | local date; year must be inferred from the session date |
| group usage months | `MMM yyyy` | "Aug 2026" | – | |
| recharge x labels | `MM/DD` | "09/29" | `RechargeCount[i].SessionStartDate` "2026-09-29T10:01:38Z" | local date; year inferred (most recent ≤ session date) |
| durations | "4 Seconds", "1.2 secs", "1.4 ms", "2.3 mins", "31 min", "1.0 hr", "2.9 days", "4 years, 1 month" | | ms / µs / hours / months in JSON | unit-aware parsing |

The PDF never prints a UTC offset or zone name. Recommended: store printed local wall-clock values as naive local
date-times plus the zone if known from context (`Europe/Zurich` for these files, from the paired JSON or site
configuration); when converting JSON UTC timestamps for comparison use the IANA zone (DST-aware), not the
session's fixed `ProgrammerUtcOffset`, and truncate (not round) the session time to minutes.

