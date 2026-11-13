# AGENTS.md — Wyss DBS Localizer Project

> **Living document.** Update this file whenever decisions are made, modules are specced, or status changes.  
> **Licence (2026-11-10):** the project is being prepared for an open-source release under **GPL-3.0-only** (`docs/LICENSING.md`; open items before publication in `docs/PHASE_HISTORY.md`, entry of 2026-11-10 (b)).  
> **State (2026-10-16):** Phases A–B **complete**; **Phase C** independent implementation on `phase-c`; full-codebase review, fixes and follow-up decisions of 2026-10-16 in `docs/PHASE_HISTORY.md`. Module status: **§12**. **Phase D** (validation) after Phase C scope closed for target milestone.

**Chat style (all agents):** use **caveman `full`** for chat in this repo unless the user switches level (`lite` / `ultra` / …) or says "stop caveman" / "normal mode". Drop articles, filler, pleasantries and hedging; fragments OK; short synonyms; technical terms, code and error strings exact. Pattern: `[thing] [action] [reason]. [next step].` Switch to full prose for security warnings, irreversible actions, or when the user is confused, then resume. Commits, PRs, and formal spec text stay normal / professional.

**Path-scoped agent rules** (`docs/agent-rules/`, plain Markdown, agent-agnostic): read the matching file before editing files under its scope.

| Rule file | Applies to |
| --------- | ---------- |
| `docs/agent-rules/phase-c-subagents.md` | `dbsloc/**`, `crates/**`, `tests/**` — independent-implementation subagent contract + mandatory prompt block |
| `docs/agent-rules/rust-safety.md` | `crates/**` |
| `docs/agent-rules/rust-gpu-perf.md` | `crates/dbsloc-viz/**` |
| `docs/agent-rules/gui-ux.md` | `crates/dbsloc-viz/**`, `crates/dbsloc-app/**` |

### Lead-Tutor API validation (Wyss DBS Localizer)

- **Dashboard `User-ID` (opaque string, not PII; decision 2026-10-24):** there is no fixed default. `dbsloc tutor submit-recon` / `submit-all-recon` / `submit-file` take `--user-id`, else `LEAD_TUTOR_USER_ID`, else the id saved in `<user data dir>/lead_tutor.json` (`DBSLOC_DATA_DIR` overrides the folder); without one, the upload asks on a terminal (Enter = random `dbsloc-<12 hex>`) or creates a random id when not interactive, and saves it, so the same user keeps one id and the Lead-Tutor web UI can compare runs over time. `dbsloc tutor user-id [--set ID | --forget]` shows or changes it; `--dry-run` / `--validate-only` never ask or save. The team's own dashboard id is `482197` (set it once per machine with `dbsloc tutor user-id --set 482197`). Allowed characters `[A-Za-z0-9._-]`. The HTTP header must match the `userid` query on the feedback URL.
- **What is sent:** `crates/dbsloc-pipeline/src/tutor.rs` (payload, validation, POST) + `crates/dbsloc-cli/src/cmd/tutor.rs` (commands) — a single **POST** to `LEAD_TUTOR_POST_URL` (default `http://tutor.lead-dbs.org:8000/my-endpoint`) with body **JSON only**: top-level keys `pt_<id>` (Lead-Tutor cohort), each value has `leftElectrode` / `rightElectrode` with **MNI** mm triplets `head`, `tail`, `x` only. `submit-file` parses and re-serializes the file and rejects unknown patient keys, unknown fields and non-numbers; `--validate-only` / `--dry-run` run the same validation. `submit-all-recon` skips a subject outside the cohort with a warning. Headers: `Content-Type: application/json`, `User-ID: <id>`. No DICOM, filenames, or other metadata in the request.

### Documentation map

| Topic | Canonical path |
| ----- | -------------- |
| Phase B specs | `specs/00_overview.md` … `specs/10_pacer.md`, `specs/09_electrode_models.md` |
| VTA / E-field from stimulation settings (`vat` stage: stimulation-settings JSON ↔ Lead-DBS `S`, native-space E-field engine, analytical models, Lead-DBS-compatible outputs, atlas statistics; rewritten 2026-10-23, implementation pending) | `specs/07_vat.md`; owner decisions `specs/14_lead_group.md` §15 |
| Lead Group (group record ↔ Lead Group `M`, `ea_stats`, batch *Calculate*, group 3D view, atlas tables, sweet spots, fibre filtering, network mapping, clinical variables; 2026-10-23, implementation pending) | `specs/14_lead_group.md` |
| Normative connectomes (catalogue, licences, polite download, Lead-DBS formats, structural / functional seed maps, per-fibre values; 2026-10-23, implementation pending) | `specs/15_connectomes.md` |
| Vendor session reports (Medtronic Percept PDF import: generic record, anonymisation with a patient-constant date shift, stimulation labels, CLI / GUI; 2026-10-23) | `specs/16_session_reports.md`; measured Medtronic layout `specs/16a_medtronic_percept_layout.md` |
| Data of an existing Lead-DBS installation (first-start question, scan of atlases and connectomes with sizes, use in place or copy; `dbsloc leaddbs scan` / `copy`; 2026-11-10) | `specs/17_leaddbs_data_import.md` |
| Checkreg / `gencheckregpair` / scroll viewer | `specs/03_coregistration.md` §12; interactive NIfTI QC `specs/08_addendum_interactive_volume_viewer.md` |
| D2/D3 + Lead-DBS GUI parity | `specs/08_visualization.md` §11 |
| Phase C roadmap / open work | `docs/PHASE_C_ROADMAP.md` |
| GPU engine cohort results | CT → anchor coreg cohort result `docs/gpu_coreg_report.md`; SyN cohort `docs/gpu_syn_cohort_report.md`; in-process N4 cohort result `docs/gpu_n4_report.md`; cross-API / vendor validation (DX12, NVIDIA, Metal) `docs/gpu_vendor_report.md` |
| Registration execution backend `--reg-backend auto\|wgpu\|ants` (behaviour, records, locks, deviations from ANTs) | `specs/04_normalization.md` §5.4 |
| Manual correction of a reconstruction (expert moves head / tail, sets the roll; pipeline writer, CLI, *Refine localization* view, lock rules, decisions 2026-10-14) | `specs/05` §9a, `specs/08` §11.1.6, `specs/12` |
| Agent rules (chat style, independent-implementation subagents, Rust / GUI standards) | `docs/agent-rules/` (index at top of this file) |
| Licensing (which licence per directory, SPDX / REUSE rules, Lead-DBS and the GPL, publishing the Apache-2.0 engine) | `docs/LICENSING.md`, `LICENSE`, `LICENSES/`, `REUSE.toml`, `NOTICE`, `fastwarp/docs/THIRD_PARTY_PORTS.md` |
| Brand (IBM Plex Sans, Wyss palette tokens, logo / window icon assets) | `docs/BRAND.md` |
| Server–client stack plan (GPU server runs the pipeline, browser client = viewer + upload + QC; API, crates, work packages) | `docs/SERVER_CLIENT_PLAN.md` |
| User-facing docs, written for outside readers and the JOSS review checklist: overview (statement of need, install, first run, shortcuts), user guide (every application area, command line, outputs, troubleshooting, Lead-Tutor self-check), contributor guide (build, test, checks, independent-implementation rule, pull requests), code of conduct. Keep the guide's labels in step with the GUI | `README.md`, `docs/USER_GUIDE.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` |
| Implementation status | **§12** below (one line per module); per-module detail `docs/STATUS.md` |
| Phase A/B/D task lists, progress tracker, dated change log | `docs/PHASE_HISTORY.md` |
| Releases (tag-driven workflow, version bump, release notes, installing a release) and the user-facing change history | `docs/RELEASING.md`, `CHANGELOG.md` (Keep a Changelog 1.1.0) |

### Documentation and checklist hygiene (required)

- **Keep docs in sync with code** whenever behavior, public API, or module status changes: update **§12** (status) and the matching row of `docs/STATUS.md` (detail, tests/notes), the dated change log in `docs/PHASE_HISTORY.md`, this file if process/stack/phase state moves, and any `specs/*.md` where behavior is locked (do not only change code).
- **Running the pipeline** — flags, environment variables, viewer launch rules, checkreg / 3D-scene defaults and the registration-engine switches are in `docs/RUN_REFERENCE.md` (normative: `specs/02`–`04`, `specs/08` + addendum). Read it before changing the CLI / `RunOptions` surface, and update it when you do.
- **README** / top-level status claims must match §12 and this file; avoid stale “done” phrasing.
- **User-facing copy** (viewer, CLI help, installer strings): avoid unexplained abbreviations in labels and buttons—prefer **directory** over “dir”, spell **window / level** where space allows, and keep mouse/MRI shorthand only in dedicated help lines where explained. Commits and internal docs stay terse as needed.

### Phase C implementation priority (current)

- **Up stack first:** BIDS + subject resolution, preprocess / coreg / norm, locks + `RunOptions` / CLI, reconstruction (and brainshift honesty). Errors, logging, and prerequisite checks matter more than new surface area.
- **Not the focus right now:** VAT and visualization—keep placeholders/MVP; extend only when upstream pipeline is stable (see **§12**).
- **Implementation vehicle:** new code and tests should **run in subagents** (see **§5 Phase C — Subagents**). Main chat orchestrates, reviews, pastes the independent-implementation block, handles commits, and only implements **after** subagent failure + retries (same section).

---

## 1. What is Wyss DBS Localizer?

`Wyss DBS Localizer` is an independent, high-performance reimplementation of the **Lead-DBS** pipeline.
Lead-DBS is a MATLAB toolbox for localizing and visualizing deep brain stimulation (DBS) electrodes in MRI/CT images of human patients.

The original source (a local checkout of [netstim/leaddbs](https://github.com/netstim/leaddbs), written `<leaddbs>` below) is **read-only reference material** — it must never be copied verbatim into this repo (see §9 Copyright Rules).

---

## 2. Goals


| #what | Goal                                | Notes                                                                                                                                                                                                                          |
| ----- | ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1     | **Eliminate MATLAB dependency**     | MATLAB is closed-source and expensive. Target must run without any MATLAB license.                                                                                                                                             |
| 2     | **Drastically improve performance** | Use compiled code; exploit CPU parallelism (multi-core) and GPU acceleration where applicable (VAT modelling, normalization).                                                                                                  |
| 3     | **Open source, GPL-3.0-only (Wyss)** | Decision of 2026-11-10: Wyss DBS Localizer is free software under the **GNU GPL version 3 only** (`LICENSE`; it was proprietary before); copyright rests with the **Wyss Center for Bio and Neuroengineering**, whose name and logo stay trademarks. The independent implementation from specs stays the rule, so that the code is the Wyss Center's own work. **Exception (2026-11-09):** the registration engine crates `fastwarp-core`, `fastwarp-niftiio`, `fastwarp-matfile`, `fastwarp-reg`, `fastwarp-gpu` are **Apache-2.0** (ITK / ANTs ports) and live in `fastwarp/` (git subtree of https://github.com/WyssCenter/fastwarp); map and rules `docs/LICENSING.md`. |


### Non-goals (for now)

- Lead Group features (VTA from stimulation settings, group analyses, connectomes): in scope since 2026-10-23, specs `07` / `14` / `15`, implementation pending; Lead-Connectome tractography and Lead-OR remain non-goals
- Lead-Connectome (fiber tractography pipeline)
- Lead-OR (intraoperative platform)
- Any GUI replication (CLI + library API first)

---

## 3. Target Language Stack

**Decision: Rust (pipeline + CLI) + wgpu (GPU) — LOCKED** (D1 of 2026-09-10, completed by unification phase 2 on 2026-10-05: there is one pipeline, `crates/dbsloc-pipeline`, behind the GUI, `dbsloc_pipeline_runner` and `dbsloc run`). **Amended 2026-10-12:** the end-user `dbsloc` CLI becomes a native Rust binary (`crates/dbsloc-cli`); Python stays as developer and research tooling only.

**Amended 2026-10-07 (GPU vehicle):** **wgpu** (Vulkan / Metal / DX12) is the primary GPU compute path for registration and visualization; CUDA is an optional later fast path. Registration engine (`fastwarp/`): crates `fastwarp-reg` (algorithms, CPU reference backend, backend trait) + `fastwarp-gpu` (wgpu device layer and WGSL kernels), selected with `--reg-backend auto|wgpu|ants`; behaviour in `specs/04` §5.4. The torch proof of concept `dbsloc/ants_gpu/` is deleted 2026-10-08.


| Layer                                                                    | Language                             | Rationale                                                                                                                  |
| ------------------------------------------------------------------------ | ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| Pipeline: BIDS resolution, stage order, locks, prerequisites, ANTs argv + process control, resampling, checkreg gate, reconstruction, `uiprefs.mat` electrode read (MATLAB v7.3 HDF5, `uiprefs.rs`), Lead-Tutor payload + POST (`tutor.rs`) | **Rust** (`crates/dbsloc-pipeline`; PyO3 module `dbsloc_reconstruct`) | One implementation for every front end; compiled, memory-safe |
| CLI (`dbsloc`: flags → `RunOptions`, progress printing, JSON summary, Lead-Tutor submit commands) | **Rust** (`crates/dbsloc-cli`; the Python CLI was removed 2026-10-12) | No pipeline logic in the CLI; calls `dbsloc-pipeline` directly |
| PyO3 module `dbsloc_reconstruct`, electrode-geometry generator, cohort scripts, library modules | **Python 3.14** (developer / research tooling) | Not needed by end users; matplotlib QC montages retired (decision C3) |
| Performance-critical algorithms (reconstruction, VAT, signal processing) | **Rust** | Compiled, memory-safe, fearless concurrency (rayon), zero-cost abstractions |
| PaCER CT reconstruction (`specs/10`) | **Rust** (`crates/dbsloc-pacer`, **AGPL-3.0-only**; optional dependency of `dbsloc-pipeline` behind the default-on cargo feature `pacer`, decision 2026-11-13) | Licensed like the original PaCER; without the feature the program is GPL-3.0-only (`docs/LICENSING.md` *PaCER and the AGPL*) |
| GPU acceleration (registration, visualization; later VAT FEM, E-field)   | **wgpu** (WGSL compute, `fastwarp/crates/fastwarp-gpu`) | One code path on NVIDIA / AMD / Intel / Apple, incl. integrated GPUs; CUDA via `cudarc` optional later fast path (amended 2026-10-07) |


### Python version policy

- Minimum: **Python 3.14**. No compatibility shims for older versions.
- Use newest language features freely: `type` statement (PEP 695), deferred annotations (PEP 649), t-strings (PEP 750), `match`, structural pattern matching, `@override`, free-threaded builds where beneficial.
- Use `**uv`** for environment and package management.

### 3a. Dependency Policy

**Minimize third-party Python dependencies.** Before adding any package:

1. Can it be done in stdlib? Use stdlib.
2. Is only 1–2 functions needed from a package? Re-implement from scratch, or copy the code if the license permits.
3. Is the package actively maintained with a large user base? If not, reject it.

**Approved / expected Python deps** (subject to revision):


| Package           | Purpose                                               | Notes                                                                                 |
| ----------------- | ----------------------------------------------------- | ------------------------------------------------------------------------------------- |
| `numpy`           | N-dimensional arrays, core math                       | Essential; well-maintained                                                            |
| `nibabel`         | NIfTI / MGH image I/O                                 | Standard in neuroimaging; well-maintained. May be replaced by Rust NIfTI crate later. |
| `scipy`           | Signal processing, spatial transforms, sparse solvers | Only specific submodules; evaluate replacing hot paths in Rust                        |


**Explicitly excluded:**

- `nilearn`, `ants-py`, `SimpleITK` — too heavy / bring in their own large dep trees
- Any unmaintained or niche package — reimplement instead
- SPM, FSL Python wrappers — call binaries as subprocesses directly

**Rust crate policy:** use well-maintained crates from crates.io; prefer `ndarray`, `nalgebra`, `rayon`, `PyO3`, `cudarc`, `nifti` (for NIfTI I/O in Rust).

---

## 5. Project Phases

### Phase A — Analysis & Specification Planning — **✅ complete** (task list: `docs/PHASE_HISTORY.md`)

### Phase B — Write Specifications — **✅ complete** (`specs/00`–`10`; task list: `docs/PHASE_HISTORY.md`)

### Phase C — Independent Implementation

**Objective:** Implement Wyss DBS Localizer from specs only, with zero reference to MATLAB source.

Rules (see §9):

- Developers implementing Phase C must not have read the MATLAB source
- All code written from specs + referenced papers only
- Each module implemented with accompanying unit tests

**Implementation branch:** `phase-c` — do Phase C work here; merge to `main` via PR when ready. (`main` = stable specs / docs; optional policy.)

**Subagents — default for implementation:** **Do not** implement feature work in the main agent by default. Spawn **subagents** (mandatory block in `docs/agent-rules/phase-c-subagents.md`) for **code and test changes**; use **readonly** explore/review subagents for discovery. Main session: planning, spec edits, gluing, git, and **fallback only** (see next sentence). In each subagent: paste the **mandatory** independent-implementation **prompt block**; read-only tools unless the task is an explicit file write; **no network** in the prompt; disable **networked** MCPs / prefer **offline** for that session. **Contracts** are `specs/*.md`. **If a subagent fails** (timeout, API limit, or tool error), **retry** at least once (narrower prompt, fewer files, or split). If retries still fail, **continue in the main session** so work does not stall. A similarity check (MOSS or similar) against Lead-DBS is **not required** (owner decision 2026-11-12: both are GPL-3, and `NOTICE` credits Lead-DBS).

Order of implementation (dependency-driven):

1. BIDS I/O + NIfTI handling
2. Coordinate space transforms
3. Electrode model definitions
4. Preprocessing
5. Coregistration (wrappers around ANTs/FSL as external processes)
6. Normalization (wrappers around ANTs/FSL/SynthMorph as external processes)
7. Electrode reconstruction (TRAC/CORE in Rust)
8. Directional orientation detection
9. VAT modelling
10. 2D/3D visualization

### Phase D — Validation & Testing

Not started: numerical / side-by-side checks vs Lead-DBS on fixtures; task list in `docs/PHASE_HISTORY.md`. CI already runs `pytest` + `cargo test` (`.github/workflows/check.yml`, `full.yml`); acceptance thresholds TBD.

---

## 9. Copyright & IP Rules ⚠️

> **This section is mandatory reading before any Phase C work.**

Lead-DBS is licensed **GPL-3.0**. Wyss DBS Localizer is **GPL-3.0-only** since 2026-11-10 (it was proprietary before); see repository `LICENSE`. The independent-implementation rules below stay in force: code that is not derived from Lead-DBS is the Wyss Center's own copyright, which keeps the licence of every part clear (the Lead-DBS bundle also contains material under non-GPL, non-commercial terms).

To keep the code free of material derived from Lead-DBS:

1. Phase A & B agents MAY read the MATLAB source to understand behavior.
2. Phase C agents / developers **MUST NOT** read, reference, or copy any MATLAB source code.
3. All Phase C implementations must be derived solely from:
  - Specifications in `specs/`
  - Referenced academic papers
  - Independent knowledge of algorithms
4. Code written after reading MATLAB source must be discarded and rewritten from the sources in rule 3.
5. External tools invoked as subprocesses (ANTs, FSL, SynthMorph) are fine — they have their own compatible licenses.
6. Bundled data (electrode model geometries, atlas ROIs) must be checked individually for license compatibility before inclusion.
7. **ANTs/ITK source code in-tree:** Vendored or modified ANTs/ITK (Apache-2.0) must keep notices and document changes, and may only live in an Apache-2.0 crate (under `fastwarp/`). See the repository `NOTICE` file (ported GPU / parity decisions) and the header template in `fastwarp/docs/THIRD_PARTY_PORTS.md`.
8. **Licence labels (REUSE, 2026-11-09):** every source file starts with an SPDX header (`Apache-2.0` for every file under `fastwarp/`, `AGPL-3.0-only` for every file under `crates/dbsloc-pacer/` (PaCER, decision 2026-11-13), `GPL-3.0-only` elsewhere); data and binaries are covered by `REUSE.toml`; licence texts in `LICENSES/`; `tests/test_licensing.py` enforces it, including that an Apache-2.0 crate never depends on a GPL-3.0-only one and that GPL-3.0-only crates reach `dbsloc-pacer` only as an optional dependency behind the default-on feature `pacer`. Builds with `pacer` (the default and the release packages) are a GPL-3.0-only + AGPL-3.0-only combination (GPL section 13); `--no-default-features` builds contain no AGPL code. Material copied from Lead-DBS (today the atlas data in `resources/atlases/leaddbs/`) carries `GPL-3.0-only AND LicenseRef-LeadDBS-Atlas-Terms`; the Wyss Center logo files are GPL-3.0-only as images, the logo and name stay trademarks (`TRADEMARKS.md`, `docs/LICENSING.md`).
9. **The `fastwarp/` subtree (2026-11-09):** `fastwarp/` is the Apache-2.0 engine, a squashed git subtree of https://github.com/WyssCenter/fastwarp with its own Cargo workspace. Edit it only in a commit that touches nothing else, with a message fit for a public repository (no patient data, no internal context), so it can go back upstream with `git subtree push --prefix=fastwarp fastwarp <branch>`; it must never depend on GPL-3.0-only code or on data of this repository. Update, contribute and check commands: `docs/LICENSING.md` *The fastwarp subtree*.

---

## 10. External Tool Dependencies

Wyss DBS Localizer wraps these tools as external subprocess calls (not reimplemented) unless the in-process wgpu engine replaces them:


| Tool              | Purpose                             | License                           |
| ----------------- | ----------------------------------- | --------------------------------- |
| ANTs              | Normalization (SyN), coregistration | Apache-2.0 ✓                      |
| FSL (FLIRT/FNIRT) | Normalization, coregistration       | FSL License (non-commercial free) |
| SynthMorph        | Learning-based normalization        | Apache-2.0 ✓                      |
| EasyReg           | Learning-based normalization        | Apache-2.0 ✓                      |
| dcm2niix          | DICOM → NIfTI conversion (bundled, `resources/dcm2niix/`) | BSD 2-clause ✓ (`NOTICE`)         |
| c3d (Convert3D)   | Image math utilities                | Apache-2.0 ✓                      |


Tools **not** wrapped (reimplemented from specs):

- TRAC/CORE reconstruction algorithm
- DiODe orientation detection
- Analytical VAT models (Kuncel, Dembek, Maedler)
- All image I/O (use nibabel / nifti-rs)

---

## 11. Development Commands

Standard invocations are defined in `pyproject.toml`, `.pre-commit-config.yaml` and `.github/workflows/{check,full}.yml` (uv, pytest, ruff, ty, cargo build / test / clippy / fmt). CI minutes are a shared org budget: `check.yml` (lint + fast tests) runs on every push to main and PR update; `full.yml` (release builds, bundled-ANTs, software-GPU, synthetic e2e, macOS behind `vars.DBSLOC_MACOS`) runs only on non-draft PRs, nightly when main moved (gate job), and on demand. Put new slow tests in `full.yml` (pytest marker `slow`), not `check.yml`. The engine in `fastwarp/` (nested workspace, excluded from the root workspace) is built, linted and tested by the same workflows with `--manifest-path fastwarp/Cargo.toml --target-dir target`; `full.yml` runs its real-ANTs tests with `DBSLOC_ANTS_DIR` set to the bundled ANTs directory. Supply chain: every third-party action in `.github/workflows/` is pinned to a full commit SHA with a trailing `# vX.Y.Z` (`actions/*` stay on major tags), CI uses `uv sync --locked` and `cargo ... --locked`, `audit.yml` runs `uv audit --frozen` and `cargo deny check advisories bans licenses sources` (`deny.toml`: crates.io only, permissive-licence allow-list, no GPL/AGPL/LGPL; workspace crates carry `publish = false`; the detached `crates/dbsloc-web` lockfile and the `fastwarp/` lockfile (`fastwarp/deny.toml`) are checked in further steps) on lockfile changes and weekly; `.github/dependabot.yml` is monthly, one combined PR across all ecosystems, cargo and uv lockfile-only, 7-day cooldown; `uv lock` ignores packages published in the last 7 days (`[tool.uv] exclude-newer` in `pyproject.toml`). The non-obvious ones:

**After adding or materially changing a feature:** build what you touched (`cargo build` / feature flags for bins you changed; `uv sync` if deps changed), then **run the corresponding end-to-end command or test runner** (e.g. the CLI invocation, binary, or `pytest` / `cargo test` slice that exercises the new behavior) and confirm it **exits successfully** (exit code 0, no unhandled error). Relying only on static checks or unrelated tests is not enough when a feature has an obvious primary entry point.

```bash
uv sync                                 # Python env incl. the dev dependency group (pytest, maturin, ruff, ty, pre-commit); there are no extras
cargo build -p dbsloc-cli --release     # the end-user CLI: target/release/dbsloc[.exe]
cargo build --locked --release -p dbsloc-cli -p dbsloc-app --no-default-features   # GPL-3.0-only build without the AGPL-3.0-only PaCER crate (recon_method pacer then fails with E_RECON_pacer_unavailable)
cargo build -p dbsloc-cli -p dbsloc-app --profile release-dev   # optimized + incremental for edit-run loops (target/release-dev/); ship and report timings from --release only
cargo build --profile debugging         # full debug info (dev keeps line tables only, deps none); Windows links with rust-lld (.cargo/config.toml), package_release.py pins link.exe
cargo build --manifest-path fastwarp/Cargo.toml --target-dir target -p fastwarp-gpu --features dev-tools   # GPU developer binaries fastwarp_gpu_probe / fastwarp_reg_syn / fastwarp_reg_ab
cargo test --workspace   # root workspace (the engine in fastwarp/ is a nested workspace, not a member)
cargo test --manifest-path fastwarp/Cargo.toml --target-dir target --workspace --exclude fastwarp-py --features fastwarp-gpu/dev-tools   # engine incl. gpu_kernels / ab_real_subject tests (CI does the same; DBSLOC_ANTS_DIR enables the real-ANTs tests)
./target/release/dbsloc run <bids> <sid> --plan   # plan / locks only, no external tools
maturin develop --release -m crates/dbsloc-pipeline/Cargo.toml   # optional extension dbsloc_reconstruct (research; some pytest modules need it)
DBSLOC_BIN=target/release/dbsloc uv run pytest   # synthetic end-to-end tests run the Rust binary
uv run python scripts/leadtutor_cohort.py compare --from-scores <ref_scores.json> <cand_scores.json>  # Lead-Tutor cohort D9 rule
uv run pre-commit install              # once per clone: commit = ruff check/format + cargo fmt; push = ty + clippy (CI parity)
uv run pre-commit run --hook-stage manual --all-files   # slow: pytest, cargo test, uv audit
cargo clippy -p dbsloc-pipeline --all-targets --features pyo3 -- -D warnings
```

## 11a. Bundled Dependencies (Single-Executable Distribution)

**Distribution:** `uv run python scripts/package_release.py` builds `dist/dbsloc-win.zip` (or `dbsloc-<platform>.tar.gz`): `dbs-localizer` and `dbsloc` (the only two programs shipped; the viewers are the `dbs-localizer checkreg` / `dbs-localizer scene` modes), the VC++ runtime DLLs, `resources/` for the platform (only git-tracked files), writes `<archive>.sha256` beside it, and tests the extracted copy with a clean environment; release builds resolve `resources/` only from the executable's ancestors or `DBSLOC_REPO_ROOT` (the compile-time source tree is a debug-build fallback). Tag `v*` → `.github/workflows/release.yml`: a draft GitHub release with the version's `CHANGELOG.md` section as notes (Keep a Changelog 1.1.0; `scripts/release_notes.py`, rotated by `scripts/bump_version.py`), then per platform the packaged folder plus installers, each tested by installing and removing it, uploaded with a `.sha256`, and the draft published only when every platform succeeded. **Release assets (2026-10-23):** Windows `dbs-localizer-setup.exe` only (WiX 7 Burn bundle around a per-user MSI, `packaging/wix/`, `scripts/build_installer.py [--test]`; installs under `%LOCALAPPDATA%\Programs` without administrator rights, Start menu + desktop shortcuts, user `PATH`); Linux `dbs-localizer_<version>_amd64.deb` (`scripts/build_deb.py [--test]`, `/opt/dbs-localizer`, `Depends` from the ELF scan) and `dbsloc-linux.tar.gz`, which ships `install.sh` (installs from the extracted folder without network, menu entry + icon from `packaging/linux/dbs-localizer.desktop`, `-u` removes); macOS only when the repository variable `DBSLOC_MACOS` is `true` (the same variable gates the macOS test and clippy jobs). `scripts/install.sh` checks the CPU architecture (Linux x86_64, macOS arm64) and, for a downloaded archive, the checksum, verifies the extracted copy in a staging folder (binaries run, bundled ANTs, shipped template) and only then swaps it in for the installed release. The GUI binary is `dbs-localizer` (package `dbsloc-app`; app id and saved settings stay `dbsloc_app`).

ANTs 2.6.5 essential binaries (`antsRegistration`, `antsApplyTransforms`, `N4BiasFieldCorrection`) ship under `resources/ants/{linux,mac,win}/`; dcm2niix v1.0.20260724 ships under `resources/dcm2niix/{linux,mac,win}/` for the import wizard (`dbsloc_pipeline::import::find_dcm2niix`: `DBSLOC_DCM2NIIX_BIN` → bundled → `PATH`); `dbsloc.bundled_bin.which_ants_binary` resolves the bundled copy first and system PATH as fallback, so every ANTs-backed stage runs offline after install (`scripts/install.sh`, public repository https://github.com/WyssCenter/dbs-localizer: `docs/RELEASING.md` *Installing a release*; `scripts/package_release.py` checks the bundle in the packaged copy). Templates: the MNI152NLin2009bAsym T1w + T2w at 1.0 mm ship in `resources/templates/space/MNI152NLin2009bAsym/res-1mm/{t1,t2,brainmask}.nii.gz` (ICBM 2009b, every second voxel of the original; the brain mask is the 2009c mask carried over by the template bridge; licence in `LICENSE` beside them and in `NOTICE`; `scripts/build_shipped_template.py`); other templates are catalogue ids (`specs/04` §3.1). **Atlases (2026-10-22):** the Lead-DBS default atlas DISTAL Minimal (Ewert 2017) ships verbatim under `resources/atlases/leaddbs/` with Lead-DBS's GPL-3 `LICENSE.md` beside it (`NOTICE`, `docs/ATLAS_DISTAL.md`; terms to be settled before a commercial release); other Lead-DBS atlases are read from `DBSLOC_LEADDBS_ATLASES_DIR` / the GUI setting, never copied. **MAT files:** `fastwarp/crates/fastwarp-niftiio` reads / writes NIfTI fast (parallel decode, parallel single-member gzip; used by the pipeline and the viewers). `fastwarp/crates/fastwarp-matfile` reads MAT v7.3 (HDF5) and writes MAT Level-5 v7 (`miCOMPRESSED`), used for `atlas_index.mat`, `uiprefs.mat` and the reconstruction `.mat` export.

---

## 12. Phase C module checklist

Status per row; code paths, tests, cohort numbers and notes for each row: **`docs/STATUS.md`** (same row numbers). Change a status here and the detail there in the same edit.

**Legend:** Done = usable for intended v1 scope · Partial = stubs, wrappers, or MVP only · Stub = placeholder / not wired to full spec · Planned = spec only, implementation pending

| #   | Spec               | Area | Status |
| --- | ------------------ | ---- | ------ |
| 1   | `01_bids`          | BIDS resolution, derivative names, image import (`bids.rs`, `derivatives.rs`, `import.rs`) | **Partial** |
| 2   | Coordinates        | Point transforms, isotropic resampling (`point_warp.rs`, `resample.rs`) | **Partial** |
| 3   | `09` electrodes    | Electrode catalogue (`electrodes/`, `electrode_catalog.rs`) | **Partial** |
| 4   | `02` preprocessing | N4 (ANTs / in-process wgpu), precoreg | **Partial** |
| 5   | `03` coreg         | CT / MR → anchor multi-start + masked rigid, checkreg gate | **Partial** |
| 6   | `04` norm          | Rigid → Affine → SyN, multispectral, `--reg-backend auto\|wgpu\|ants` | **Partial** |
| 7   | `05` recon         | TRAC/CORE + PaCER in Rust (PaCER: `crates/dbsloc-pacer`, AGPL-3.0-only, default-on feature `pacer`), manual correction, `.mat` export | **Partial** |
| 8   | `06` orientation   | DiODe (Boston / Abbott directed leads) | **Experimental** |
| 9   | `07` vat           | Stimulation settings, CPU + wgpu E-field engine, `vat` stage | **Partial** |
| 10  | `08` viz           | `dbsloc-viz`: checkreg, 3D scene, refine view, backdrops, atlas sets, fibre / map layers, group view | **Partial** |
| 11  | `08` GUI           | `dbsloc-app` (`dbs-localizer`) | **Partial** |
| 12  | `14` group         | Lead Group record + twin, *Calculate*, group statistics, `dbsloc group` | **Partial** |
| 13  | `15` connectomes   | Catalogue, download, readers, seed maps, cache | **Partial** |
| 14  | `16` reports       | Vendor session-report import (Medtronic Percept) | **Partial** |
| 15  | `17` Lead-DBS data | Import of atlases / connectomes from a Lead-DBS installation (`leaddbs_data.rs`, `dbsloc leaddbs`, first-start window) | **Partial** |
| —   | Orchestration      | `dbsloc-pipeline` plan / run / locks / batch | **Partial** |
| —   | Validation         | tutor + Lead-Tutor | **Partial** |
| —   | Native             | `dbsloc_reconstruct` PyO3 module (research only) | **Optional** |
| —   | CI                 | `.github/workflows/` check, full, release | **Partial** |

**Electrode geometry is catalogue-driven on both paths** (roadmap 2.1–2.3, 2.10). `dbsloc/electrodes/layout.py` resolves any of the 73 catalogue rows to per-level depths and per-segment azimuths (`specs/09` §3.1–§3.2); `crates/dbsloc-reconstruct/src/electrode_catalog.rs` embeds the same geometry, generated by `scripts/emit_electrode_geometry.py` and drift-guarded by `tests/test_electrode_geometry_export.py` — do not port the derivation, re-run the emitter. Neither path has a default lead: an unresolved `el_model` fails the stage, because the previous fallback (1.5 mm pitch, 4 contacts) matches no real device and 9 of the 10 Lead-Tutor subjects are 8-contact directional.

*Dated change history: `docs/PHASE_HISTORY.md`.*


---

## 15. References

- Horn et al. (2019). Lead-DBS v2: Towards a comprehensive pipeline for deep brain stimulation imaging. *NeuroImage*, 184, 293–316.
- Husch et al. (2017). PaCER — A fully automated method for electrode trajectory and contact reconstruction in deep brain stimulation. *NeuroImage: Clinical*, 17, 80–89.
- Dembek et al. (2017). Probabilistic mapping of deep brain stimulation effects. *NeuroImage: Clinical*, 16, 1–9.
- Schönecker et al. (2009). Fascicular connectivity of the subthalamic nucleus. *Journal of Neurology, Neurosurgery & Psychiatry*, 80, 1046–1049.
- Avants et al. (2008). Symmetric diffeomorphic image registration with cross-correlation. *Medical Image Analysis*, 12(1), 26–41.
- User guide: [https://netstim.gitbook.io/leaddbs](https://netstim.gitbook.io/leaddbs)
- Source reference (read-only): a local checkout of https://github.com/netstim/leaddbs (`<leaddbs>`)

