# Server–client stack — implementation plan

**Date:** 2026-10-10. **Status:** WP1–WP5 implemented on `main` (uncommitted as of §11); WP0/WP6–WP8 pending. **Decision it follows:** the WebAssembly feasibility study of 2026-10-10 (compiling the pipeline to WebAssembly is not practical; a browser *viewer* is). **Premise from the project:** the backend server always has a GPU, so the server runs the existing Rust pipeline with `--reg-backend wgpu` and needs no ANTs binary. The browser client is the viewer: sign-in, upload, run control, QC approval, 3D review, downloads.

Facts about the code as it is today carry `file:line`. Estimates are for one developer who knows the tree.

## 0. Decisions taken (2026-10-10, with the project owner)

| # | Decision | Consequence |
| --- | --- | --- |
| D1 | **GPU vendor is not a code decision.** Code stays vendor-agnostic (wgpu); likely first deployment is Linux + NVIDIA; validation on the real server GPU is part of WP7. | WP0 holds no hardware question. |
| D3 | **Browser client = egui compiled to wasm** (`dbsloc-web` on eframe `WebRunner`), sharing the landing / guided / card state machines with `dbs-localizer` through `dbsloc-ui`, and the viewer through `dbsloc-viz` `web`. | One Rust codebase, same look as the desktop app; accepts egui-web limits (canvas text, accessibility via AccessKit only partially) for a clinical operator tool. |
| D4 | **The interactive viewer is part of v1.** No release before WP5: checkreg review in WebGPU (window / level, slice scrolling, overlay) and the 3D scene, approve / reject from the viewer. | WP5 moves before the v1 cut; first usable version 9–11 weeks. PNG slides remain the fallback when WebGPU is unavailable. |
| D5 | **Server-side DICOM import in v1.** Upload a DICOM folder or zip; the server runs `dcm2niix` (MIT, bundled) and proposes the BIDS mapping from the series descriptions; NIfTI upload stays. | WP3 grows by about a week and bundles the `dcm2niix` binary per platform; the upload API is "bundle of files + commit mapping" for both routes. |
| D6 | **No ANTs on the server.** `reg_backend = wgpu` always; a failed device self-test fails the run instead of falling back. | No `resources/ants/` in the server artefacts; `--allow-ants` is dropped from the plan. |
| D7 | **Precomputed AHEAD meshes are committed**: the ten `.obj` files under `resources/atlases/ahead/structures_mni09b/precomputed_obj/` (4 MB), **not** the `.cache.json` sidecars, which record an absolute Windows path and an mtime (`atlas_marching.rs:124-133`, `:430`) and are meaningless on another machine. | The server ships the `.obj` files and the web viewer meshes nothing. WP5 changes the desktop cache key from path + mtime to a content hash of the mask so a committed `.obj` is accepted on every checkout; until then the desktop viewer re-meshes once per machine as today. **Done 2026-10-15:** the key is the mask's sha256 (+ template sha256 + meshing parameters, `atlas_marching.rs` key v4), the ten sidecars are committed, and new meshes go to the per-user cache (`%LOCALAPPDATA%\dbsloc\cachetlas_meshes`, `~/Library/Caches/dbsloc/atlas_meshes`, `$XDG_CACHE_HOME/dbsloc/atlas_meshes`), never into `resources/`. |
| D8 | **Bare install + `systemd`** on the server; Docker not in v1. | Tarball with binaries, templates, atlas, web `dist/`; install script + unit + `config.toml`; the host driver provides Vulkan. |
| D9 | (technical, taken by the implementer) **One `dbsloc_pipeline_runner` subprocess per job.** | Driver panics and device loss kill one job, not the server; one device creation + self-test per job. |
| D2 | **No authentication in v1; trusted network only.** The server binds to a LAN / VPN address and trusts every caller. | Prototype / research deployments only. Before patient data from the clinic goes on the server, WP6 (auth, roles, ACL) must be done. The audit log records the client address instead of a user; every route goes through one middleware so WP6 adds auth without touching handlers. **Request guard (2026-11-12):** a Host / Origin check runs before every route against DNS rebinding and cross-site requests (§7). It is not authentication: anyone who can reach the server can still use it. |

## 1. Architecture

```
 browser (Chrome / Edge / Firefox / Safari, WebGPU)          GPU server (Linux)
 ┌──────────────────────────────┐   HTTPS JSON + SSE    ┌──────────────────────────────────────┐
 │ dbsloc-web (wasm32, eframe)   │◄────────────────────►│ dbsloc_server (axum, tokio)            │
 │  sign-in · datasets · upload │   Range GETs (files)  │  REST /api/v1 · SSE events · auth     │
 │  guided run · progress · QC  │                       │  job queue (one GPU job at a time)    │
 │  web viewer (dbsloc-viz web)  │                       │   └─ dbsloc_pipeline_runner (subprocess│
 └──────────────────────────────┘                       │      per job, RunOptions in, NDJSON   │
                                                        │      RunEvent out, lib.rs:300-331)    │
                                                        │  BIDS data root on local disk         │
                                                        │  wgpu device (Vulkan) · templates ·   │
                                                        │  AHEAD atlas · gencheckregpair_png     │
                                                        └──────────────────────────────────────┘
```

- **The server is a remote `dbsloc run`.** Same `RunOptions` (`options.rs:41`), same stage driver, same locks, same `derivatives/` layout, same checkreg manifest (`checkreg.rs:314`) and scene manifest (`scene_manifest.rs:20`). Nothing new on disk; every file the client shows is one the desktop app already produces.
- **Headless by construction.** `headless = true`, `checkreg = true` → PNG slides and the manifest are written, no viewer process is spawned (`checkreg.rs:385-432`). The QC gate's "not approved" error becomes a typed *awaiting QC* state (§4.3).
- **One process boundary per job.** The queue spawns `dbsloc_pipeline_runner` (`bin/dbsloc_pipeline_runner.rs`) through `run_pipeline_subprocess` (`lib.rs:300`), so a driver panic or device loss kills one job, not the server. The cost is one device creation + self-test per job (~1 s on the Arc; `reg_backend.rs` shares one device *within* a process, which still saves the three further creations per run).
- **Desktop app stays.** `dbs-localizer` is unchanged; WP8 optionally lets it talk to a server.

## 2. Crates

| Crate | Target | Role |
| --- | --- | --- |
| `crates/dbsloc-api` (new, lib) | native + wasm32 | DTOs shared by server and client: `RunRequest` (the client-settable subset of `RunOptions`), `SubjectSummary`, `RunStatus`, `QcSlide`, `UploadPlan`; re-exports `dbsloc_pipeline::protocol::RunEvent` as serde JSON (no `std::fs`, no `PathBuf` in any DTO — paths become opaque file keys). |
| `crates/dbsloc-ui` (new, lib) | native + wasm32 | UI state machines pulled out of `dbsloc-app` that have no fs dependency: the guided step model (`guided.rs`), `StageSettings` / `stage_options`, landing-card state, the stage-card copy. `dbsloc-app` and `dbsloc-web` both depend on it. |
| `crates/dbsloc-server` (new, bin `dbsloc_server`) | native | axum + tokio: auth, datasets, uploads, runs + queue + SSE, QC approval, file serving, GPU probe. Depends on `dbsloc-pipeline` (no `pyo3`), `dbsloc-api`. |
| `crates/dbsloc-web` (new, bin → wasm) | wasm32 | eframe `WebRunner`; `dbsloc-ui` + `dbsloc-viz` (`web` feature) + `dbsloc-api`; HTTP via `ehttp`, SSE via `web-sys` `EventSource`. Built with `trunk`. |
| `crates/dbsloc-viz` | + `web` feature | byte-based loaders and no-thread paths (§6). |
| `crates/dbsloc-pipeline` | small changes | typed QC-pending outcome, `apply_approval` moved into the lib, subject scan DTO (§4.3, WP1). |

## 3. Data on the server

```
<data_root>/
  datasets/<dataset>/            one BIDS root per dataset (what `--bids-dir` points at today)
    rawdata/sub-<id>/…           unchanged BIDS layout; `rawimages.json` as today (bids.rs)
    derivatives/leaddbs/sub-<id>/…   unchanged (locks/, coregistration/, normalization/, viz/)
    runs/<run-id>.ndjson         persisted RunEvent stream for replay and audit
  incoming/<upload-id>/          staged uploads until commit (§4.2)
  server/
    config.toml                  bind, data_root, template paths, gpu_jobs, auth mode
    users.toml | oidc            local accounts (v1) or OIDC (WP6)
    audit.log                    who ran / approved / uploaded what, when
```

`resolve_subject_with` (`bids.rs`) and `list_subject_ids` are the subject index; the server never keeps a second catalogue.

## 4. API (`/api/v1`, JSON; errors `{code, message}` with the pipeline's own `E_*` codes where they exist)

### 4.1 Read

| Method | Path | Returns | Backed by |
| --- | --- | --- | --- |
| `GET` | `/gpu` | adapter, graphics API, self-test verdict | `reg_backend::cached_probe` |
| `GET` | `/datasets` | datasets the user may see | config + ACL |
| `GET` | `/datasets/{ds}/subjects` | ids | `list_subject_ids` |
| `GET` | `/datasets/{ds}/subjects/{sid}` | `SubjectSummary`: resolved files (`Subject::to_json`), `modalities` and `postop_modalities`, prerequisites, existing outputs, lock / approval state per stage | `resolve_subject_with`, `prereqs`, `locks::read_approval`, the scan `dbsloc-app` does today (`panels.rs`, moved to the lib in WP1) |
| `POST` | `/datasets/{ds}/subjects/{sid}/plan` | `RunPlan` (what would run / skip and why) | `plan_run` (`lib.rs:98`) |
| `GET` | `/datasets/{ds}/subjects/{sid}/qc/checkreg` | manifest with `checkreg_png` / `moving` / `fixed` as file keys, plus approval state per slide | `checkreg_manifest.json` (`checkreg.rs:71`), `method_log_allows_continue` |
| `GET` | `/datasets/{ds}/subjects/{sid}/viz/scene` | scene manifest with file keys | `viz/scene_manifest.json` |
| `GET` | `/datasets/{ds}/subjects/{sid}/files/{key}` | the file, `Range` supported, `Content-Type` by extension | allowlist: `rawdata/sub-<id>/**`, `derivatives/**/sub-<id>/**`, templates, atlas directory; no `..`, no symlinks (same rule as `bids.rs`) |
| `GET` | `/runs/{id}` | `RunStatus` (queued / running / awaiting_qc / done / failed / cancelled / resumed / rejected, current stage, k-of-N, started, elapsed, `gated_stage`, `resumed_by`) | queue |
| `GET` | `/datasets/{ds}/subjects/{sid}/runs/active` | the subject's queued / running / awaiting_qc `RunStatus`, or `null` (restores the current run after a page reload) | queue |
| `GET` | `/runs/{id}/events` | SSE of `RunEvent`, replayed from the `.ndjson` then live; works for queued runs and after a restart; every message carries `id: <seq>` and `Last-Event-ID` is honoured; ends with a named `end` event whose data is the terminal `RunStatus` (an `awaiting_qc` run ends its stream too); keep-alive comments | `runs/<id>.ndjson` |

### 4.2 Write

| Method | Path | Body → effect |
| --- | --- | --- |
| `POST` | `/auth/login`, `/auth/logout` | **WP6, not in v1 (D2)**: session cookie (`SameSite=Strict`, `HttpOnly`), local accounts or OIDC |
| `POST` | `/datasets/{ds}/uploads` | `{files: [{name, size, sha256}]}` → upload id + per-file chunk URLs; a file of size 0 is rejected; uploads abandoned for more than 24 h are swept at server start and on each new upload |
| `PUT` | `/uploads/{id}/{file}/{chunk}` | raw chunk (8 MiB), resumable; server verifies the sha256 at the end. The parsed `upload.json` and a name → file index are cached per upload id (reloaded when the file's size or mtime changes, dropped on commit; at most 256 uploads), so a chunk costs one `stat` instead of a manifest parse and a linear search (2026-10-16) |
| `POST` | `/uploads/{id}/convert` | **D5**: for a DICOM upload, runs the bundled `dcm2niix` through `dbsloc_pipeline::import::convert_dicom` (`-z y -b y -f %s_%p_%d`, `specs/01` §8.4) and returns `Vec<ConvertedSeries>` (`dbsloc-api`: `file`, `series_number`, `description`, `protocol`, `modality` MR / CT, `dims`, `voxel_mm`, `mapping` = the shared suggestion with `session`, `suffix`, `acq`); the client edits it before the commit (2026-10-14) |
| `POST` | `/uploads/{id}/commit` | `{subject, mapping: {file → {session (required: preop / postop), modality: "anat", suffix, acq?}}, replace_existing?: bool}` → header scrub (server) then `dbsloc_pipeline::import::{plan_import, run_import}` (`specs/01` §8.4, 2026-10-14): files land as `.nii.gz` with `acq-` in the name, sidecars beside converted series are copied, post-op rows carry the `modality` entity, conflicts return 409 `E_IMPORT_CONFLICT` unless `replace_existing`, rule violations 400 `E_IMPORT_RULES`, bad files `E_NIFTI`, a missing session `E_BAD_SUBJECT`; `resolve_subject_with` must succeed or everything is rolled back; `E_DUPL_RAW` and the other BIDS errors are returned as is. A commit while the subject has a queued, running or awaiting-QC run returns 409 `E_RUN_ACTIVE`. Response `{subject, files, invalidated}` with dataset-relative paths; `invalidated` is the invalidation line of a replacing import (`specs/01` §8.4) or `null`. Files produced by `/convert` (listed in `converted.json`) are committed by name without the announced-sha check; every other file must be announced with a matching sha256 |
| `POST` | `/datasets/{ds}/subjects/{sid}/runs` | `RunRequest` → run id. The server builds `RunOptions`: `bids_dir`, `normalization_template` (+ T2), `headless = true`, `checkreg = true`, `no_scene3d = true`, `reg_backend = Wgpu` and `stop_on_error = true` are set by the server, and the runner starts without the server's `DBSLOC_*` run-option variables (only tool / data locations, the template choice `DBSLOC_TEMPLATE_T1` / `DBSLOC_TEMPLATE_T2` that `/plan` also reads in process, and `DBSLOC_GPU_*` are kept, `runs::JOB_ENV_KEEP`, `docs/RUN_REFERENCE.md`, 2026-10-16); the client may set `stages`, `norm_method` (validated, run as `ants_syn`), `norm_isotropic_mm` (0.3–3.0 mm, else 400 `E_BAD_REQUEST`), `norm_full_grid`, `norm_multispectral`, `recon_method`, `recon_bright_metal`, `el_model`, `overwrite_approved`; `RunRequest` has no `ants_extra` and unknown fields are ignored |
| `POST` | `/runs/{id}/cancel` | queued / awaiting_qc → `cancelled` at once; running → the runner gets a `cancel` line on stdin, stops its own tools, and is killed after a 10 s grace period (`run_pipeline_subprocess_cmd`) |
| `POST` | `/datasets/{ds}/subjects/{sid}/qc/checkreg/{slide}/approval` | `{ok: bool}` → 200 `ApprovalResponse {all_approved, resumed_run}`. Writes the method-log approval as the viewer does (`checkreg::apply_approval`: per modality for coreg slides, one scalar for normalize / brainshift); the method log must lie under the subject directory and the slide branch must match the awaiting run's gate. While a run of the subject is `running`, or a fresh (non-resume) run is `queued`, the approval returns 409 `E_RUN_ACTIVE` (approving the slides of an `awaiting_qc` run is the normal flow); when the running run has already reported its gate (`gated_stage` set) the request waits up to 15 s for the runner to exit and the job to become `awaiting_qc`, so an approval sent on the SSE gate event is not refused. Approvals of one subject are serialised by an in-process lock around the read-modify-write of the method log and of the scalar slide decisions (`<method log>.qc-slides.json`, `{log_sha256, slides}`: the decisions are valid only while the method log's sha256 equals the recorded one, so a re-run that rewrites the log starts a fresh set; 2026-10-16). A reject ends the awaiting run as `rejected`. When every slide is approved the awaiting run is claimed atomically and retired as `resumed` (a resume job runs the remaining stages, `resumed_run`) or `done` when nothing remains |
| `POST` | `/datasets/{ds}/subjects/{sid}/locks/{stage}` | `{approved: bool}` → `locks::write_approval` (what `dbsloc lock approve` does) |

### 4.3 The QC gate on a server

Today the gate runs in the stage driver after the stage body has committed its outputs (`stages.rs:126-142`; outputs are renamed from `.partial-*` by `staged.finish`, `stage_impl.rs:1899`). Headless and not pre-approved, `run_norm_checkreg` returns `Err("norm: qc not approved — …")` (`checkreg.rs:427`) and the run stops with `StageFailed`. The outputs are complete on disk, so nothing is lost; only the *signal* is wrong for a server.

WP1 makes the signal typed, without changing the CLI tokens:

- `PipelineError::QcPending { stage, manifest }` returned by the gate instead of the string error; the driver emits a new `RunEvent::StageAwaitingQc { stage }` and stops the run as it does now. `dbsloc run` prints the same `error: … qc not approved` row and keeps exit code 1 (`tests/test_cli_run.py` unchanged); the GUI log shows the same line.
- The server maps `StageAwaitingQc` to run status `awaiting_qc`; after the approval `POST`s it submits a new runner job for the stages after the gated one. Stage locks are untouched, so an approved normalize is not re-run.

### 4.4 Queue

- One worker per GPU job slot; `gpu_jobs = 1` by default (the 1.0 mm normalize peaks at 1.45 GB device memory after the 2026-10-09 perf pass; a 16 GB server card could run 3–4, but the runner's N4 and SyN use every CPU core as well, so measure before raising it).
- FIFO per dataset, with "resume after QC" jobs placed at the head.
- Jobs are `RunOptions` + user + timestamps, persisted in SQLite (`rusqlite`, bundled) so a server restart re-queues what was queued and marks what was running as failed.

## 5. Client (`dbsloc-web`)

Screens, in the order a user meets them; the guided flow is the desktop one (`guided.rs`), driven by server data instead of the local scan:

1. **Sign in.**
2. **Landing** (the desktop `landing.rs` card): datasets, recent subjects (server-side per user), *Upload subject…*, *Open subject…*.
3. **Upload**: drag-and-drop NIfTI files or a zip of a BIDS subject; the client reads the NIfTI header in the browser (`nifti` on wasm, header only) and proposes the BIDS mapping (modality from the file name entities, session), the user confirms, chunks go up with a progress bar; commit errors show the pipeline's BIDS message.
4. **Guided steps** Subject → Preprocess → Coregister → Normalize → Reconstruct → Review: the same cards (`dbsloc-ui`), each judged against `/plan` and `SubjectSummary`; *Run* posts a run for that step, *Next* continues with the existing output.
5. **Progress**: SSE log, k-of-N, elapsed, cancel. Reconnect replays the `.ndjson`.
6. **QC** (when a run is `awaiting_qc`): the checkreg PNG slides with *Approve* / *Reject* per slide (v1, PNG only); the interactive volume review (window / level, slice scroll, overlay) comes with the web viewer in WP5.
7. **Review**: the 3D scene (template slices, electrode bodies, atlas) in WebGPU; downloads: reconstruction JSON, checkreg PNGs, `Composite.h5` / `InverseComposite.h5`, warped CT.

Persistence: `eframe` web saves `App::save` to `localStorage` (theme, dock, last dataset). No PHI goes into `localStorage`: subject ids only, which are the BIDS labels the user chose.

Brand: `dbsloc_viz::brand` is `interactive-eframe` only and `include_bytes!` the fonts and icons, so the web build carries the same 600 KB of Plex and the same `ui_tokens` palette; nothing to redo.

## 6. `dbsloc-viz` `web` feature (the viewer port)

1. **Loaders take bytes.** `SceneVolume::load(path)` (`scene_volume.rs:20`) and `load_mni_side_geometry(path, side)` (`reco_json.rs:174`) gain `from_bytes` / `from_str` variants over `nifti::ReaderOptions::from_reader` (gzip through `flate2` with the `miniz_oxide` backend, pure Rust); the OBJ atlas loader (`atlas_wavefront.rs`) takes a string. The path variants stay for the desktop.
2. **Manifests with keys.** `SceneManifestV1` and the checkreg manifest keep their shape; on the web the `PathBuf` fields are file keys resolved against `/files/{key}` (the server rewrites them on `GET …/viz/scene` and `…/qc/checkreg`).
3. **No threads.** The ROI mesher (`scene3d_gpu.rs:826`), the atlas loader (`:1148`, `:1262`) and the screenshot encoder (`:3838`) run on the main thread in time-sliced chunks under `cfg(target_arch = "wasm32")`, or are avoided: the server ships the **precomputed AHEAD meshes** (`resources/atlases/ahead/structures_mni09b/precomputed_obj/`, 10 `.obj` + cache JSON, generated by the desktop viewer's cache) so the web viewer meshes nothing. `rayon` is behind `cfg(not(wasm32))`.
4. **Input.** `gesture_zoom::split_wheel_events` decides touchpad vs wheel from fractional `Line` deltas (a winit-on-Windows fact); eframe web delivers `Point` deltas with the browser's `deltaMode`, which already maps to `ScrollIntent::Pan`. Verify on a Mac trackpad in Safari and Chrome.
5. **Headless render helpers** (`depth_pass.rs`, `mni_slice.rs`, `readback.rs`, `wgpu_headless.rs`) block on `device.poll` and stay native-only; the interactive scene draws through `egui_wgpu::Callback` and needs no change.
6. **Interactive checkreg in the browser** (the `dbs-localizer checkreg` window logic → a `checkreg_view` module shared by the desktop app and the web): two volumes per slide, the fixed one resampled on the moving grid as today, window / level as a shader uniform, the approve / reject buttons calling the API instead of `apply_approval`.

Size budget: the 1 mm resampled template and the warped anchor are ~8 MB each gzipped, the CT ~30 MB; the AHEAD meshes ~5 MB. First paint of the review screen within a few seconds on a hospital LAN.

## 7. Security and compliance (not optional for patient data)

- TLS always; the server binds `127.0.0.1` by default and sits behind Caddy / nginx for certificates, or uses `rustls` directly with a configured certificate.
- Auth: **none in v1 (D2)**, trusted network. WP6 adds accounts (local Argon2 or OIDC against the hospital identity provider), roles *viewer* (read, download), *operator* (upload, run, approve), *admin* (users, datasets), and dataset-level ACL, behind the one middleware that v1 already routes everything through.
- Request guard (2026-11-12, `request_guard_middleware` in `crates/dbsloc-server/src/middleware.rs`, the outermost layer). It protects the server without authentication against two browser attacks: a hostile web page whose DNS name is rebound to the server's address (DNS rebinding), and a hostile page that sends a write request to the server (cross-site request forgery). It is **not** authentication: a program that can reach the server can still read and write everything (D2).
  - Host rule, every request: the `Host` header (or the URI authority when there is no `Host` header) must name an IP address (IPv4, or IPv6 in brackets), `localhost`, or a name listed in the config key `allowed_hosts` (default empty). Each entry is a host name only: no port, scheme, path or wildcard; the server refuses to start on any other entry. Entries are compared case-insensitively and a trailing dot is ignored. Anything else, including a malformed `Host`, gets `403` with code `E_HOST`. A request without any host passes (in-process tests; browsers always send one).
  - **A server reached by a DNS name must list that name in `allowed_hosts`**, for example `allowed_hosts = ["dbs.lab.example"]`; reached by its IP address it needs nothing.
  - Origin rule, every method except `GET`, `HEAD` and `OPTIONS`: an `Origin` header, when present, must be the server's own origin or be listed in `cors_origins`. Same origin means the `host[:port]` of the `Origin` equals the `Host` value; the scheme is not compared, so a server behind a TLS proxy (`https` outside, `http` inside) still works as long as the proxy passes the original `Host`. `Origin: null` is always refused. `Sec-Fetch-Site: cross-site` is refused unless the `Origin` is listed in `cors_origins`. A refusal is `403` with code `E_ORIGIN`. Requests without `Origin` and without `Sec-Fetch-Site` (`curl`, scripts, the CLI) pass.
  - A reverse proxy must pass the original `Host` (Caddy does by default; nginx needs `proxy_set_header Host $host;`) and must forward only requests for its configured server name. A proxy that rewrites `Host` to the inner address disables the rebinding check, because every request then names an IP address. Do not work around refused writes behind such a proxy with `cors_origins`; fix the proxy.
  - A development client on an origin listed in `cors_origins` (for example `http://localhost:8081`) must still reach the server by an allowed host, such as `127.0.0.1`, `localhost` or an `allowed_hosts` name. In the browser a refused host shows up as a CORS failure, because the `403` carries no CORS headers.
  - Every refusal writes one line to stderr: method, peer address, code and the refused value (quoted, escaped, cut at 100 characters). The response never echoes the value. There is no rate limit on these lines.
- Audit log: every upload commit, run submission, cancel, approval and lock change with user, subject, time; append-only file plus the SQLite job table.
- File serving: allowlisted roots, canonicalised paths, no symlink traversal (the same rule `bids.rs` applies to directory walks), `Content-Disposition: attachment` for downloads.
- No absolute server paths in API output (2026-10-16, `crates/dbsloc-server/src/sanitize.rs`): one sanitizer replaces the datasets directory (`<datasets>/<name>/…`, also a dataset's canonical root when it is a symlink), upload staging (`<uploads>`), `server/` (`<server>`), the data root (`<data>`), `templates_dir` (`<templates>`), `atlas_dir` (`<atlas>`), `web_dir` (`<web>`), the install / resources directory (`<install>`), the per-user data directory (`<user-data>`), the temp directory (`<temp>`) and the home directory (`<home>`, last resort) in every JSON response under `/api`, every JSON, `text/plain` or untyped error body (an HTML error body passes unchanged), and every SSE event and `end` status (all string values and keys); both separators, `\\?\` verbatim and canonical forms are matched, case-insensitively on Windows, and on Windows the separators after a token become `/`. Scene manifests rewrite every absolute path string (any key, nested) to a file key or `""`. The server log (stderr, job table, run NDJSON) keeps full paths; a 5xx body is printed to stderr before it is sanitized. Downloads (`Content-Disposition: attachment`) are served byte for byte, so pipeline JSON files fetched through `/files/` still contain the paths written by the pipeline.
- No PHI in logs: subject labels and file names only. NIfTI headers can carry `descrip` / `aux_file` strings from the scanner; the upload commit blanks both (a few bytes in the header, documented).
- Session cookies `HttpOnly; Secure; SameSite=Strict`; CSRF covered by same-site plus a custom header on writes. No COOP/COEP needed in v1 (no `SharedArrayBuffer`).
- Resource limits: upload size per file (2 GB), per-dataset quota, one run per subject at a time, request body limits on JSON.
- Backups and retention are the operator's; the plan documents what to back up (`datasets/`, `server/`).

## 8. Deployment

- Artefacts: `dbsloc_server`, `dbsloc_pipeline_runner` (the check images are drawn in-process by `gencheckregpair_png` since 2026-10-23, no helper binary), the `dbsloc-web` `dist/` (served by `dbsloc_server` itself at `/`), templates (`resources/templates/…`), the AHEAD atlas with the precomputed meshes. No ANTs binaries (D6); `dcm2niix` for Linux is bundled (D5). Start-up refuses to serve when `dbsloc gpu probe` finds no hardware adapter or the self-test fails.
- `config.toml` of a server that users reach by a DNS name lists that name in `allowed_hosts`; without it every request by that name gets `403 E_HOST` (request guard, §7).
- Linux server with a Vulkan driver (likely NVIDIA proprietary for the first deployment, D1; Mesa RADV / ANV otherwise). - `systemd` unit, `config.toml`, a `scripts/install_server.sh` (D8). Docker is a later option (needs the vendor's GPU runtime and the Vulkan ICD mounted; the Vulkan-in-container path must be validated per vendor).
- CI: `cargo test -p dbsloc-server` (axum integration tests on the synthetic subject of `tests/synthetic_head.py`, `WGPU_FORCE_FALLBACK_ADAPTER=1` for the API tests that do not register), `cargo build --target wasm32-unknown-unknown -p dbsloc-web` plus `trunk build`, `cargo clippy` for both targets, `wasm-bindgen-test` for the loaders.

## 9. Work packages

| WP | Scope | Depends on | Estimate | Acceptance |
| --- | --- | --- | --- | --- |
| **0 Decisions** | Choose auth mode (local / OIDC), DICOM in or out (out for v1; `dcm2niix` is MIT and could run server-side later), confirm subprocess-per-job. The GPU vendor is **not** a decision: the engine is wgpu on Vulkan / DX12 / Metal with a self-test on every device, so the server hardware is chosen at deployment and validated there (WP7) | — | 1–2 d | decisions recorded in this file |
| **1 Pipeline + shared crates** | `dbsloc-api` DTOs; `dbsloc-ui` extraction (`guided.rs` step model, `StageSettings`, card copy) with `dbsloc-app` moved onto it; `PipelineError::QcPending` + `RunEvent::StageAwaitingQc`; `apply_approval` into `dbsloc_pipeline::checkreg`; the subject scan (`panels.rs`) into the lib as `SubjectSummary`; `run_pipeline_subprocess` with a cancel handle | 0 | 1 wk | all existing tests unchanged and green; `dbs-localizer` behaves as before (screenshots); new unit tests for the DTOs and the typed gate |
| **2 Server core** | axum app, config, one pass-through middleware (auth seam for WP6), datasets / subjects / plan / files (allowlist, Range), runs + SQLite queue + runner subprocess + SSE replay + cancel, QC approval + resume, lock approval, GPU endpoint, audit log | 1 | 2 wk | integration test: upload-free run on the synthetic subject through the API with `--reg-backend wgpu`, `awaiting_qc` → approve → resume → reconstruction JSON downloadable; same per-stage results as `tests/test_e2e_synthetic_wgpu.py` |
| **3 Uploads + DICOM** | chunked resumable upload, sha256, NIfTI header validation and header scrub, BIDS placement + `rawimages.json`, rollback, quotas; DICOM folder / zip → bundled `dcm2niix` → series list with a proposed mapping (D5) | 2 | 2 wk | upload the synthetic subject from a test client, `resolve_subject` returns the same files as the on-disk copy; duplicate and malformed cases return the BIDS errors |
| **4 Web client v1** | `dbsloc-web` on eframe `WebRunner`: sign-in, landing, upload UI, guided steps on `dbsloc-ui`, progress with SSE, QC with PNG slides, downloads; `trunk` build served by the server | 1, 2, 3 | 2 wk | manual script on Chrome / Edge / Firefox; headless-Chrome screenshot of each screen in CI (optional); the synthetic subject runs end to end from the browser |
| **5 Web viewer (in v1, D4)** | `dbsloc-viz` `web` feature (§6): byte loaders, manifests with keys, no-thread paths, precomputed meshes, 3D scene and interactive checkreg in WebGPU, approve / reject from the viewer | 2, 4 | 2–4 wk | the synthetic subject's scene and checkreg slides render in the browser; WebGPU unavailable → PNG fallback; `wasm-bindgen-test` for loaders |
| **6 Auth + hardening** | OIDC, roles and dataset ACL enforced on every route, audit log review, rate limits, TLS via `rustls` or proxy docs, dependency audit (`cargo deny`) | 2, 4 | 1 wk | route table with required role, tests per route for 401 / 403; a short threat-model page |
| **7 Deployment + server GPU validation** | `systemd` unit, install script, config reference, backup / restore doc (D8; no Docker in v1), CI jobs for server + wasm; **Lead-Tutor cohort on the actual server GPU** (`scripts/leadtutor_cohort.py`; `docs/gpu_vendor_report.md` lists NVIDIA / AMD as pending) | 2–6 | 3–5 d + cohort time | fresh Linux machine with a GPU: install → probe → synthetic subject from a browser on another machine; D9 passes on the server GPU, two runs byte-identical, report row added |
| **8 (optional) Desktop remote mode** | `dbs-localizer` gains a *Server* setting: with it set, the subject list, runs and files come from the API through the same `dbsloc-ui` state machines; the local pipeline stays the default | 4 | 1 wk | the desktop app drives a server run end to end |

Total for v1 (WP0–WP7 with the interactive viewer and DICOM import): **9–12 weeks** of one developer; WP5 is the widest range. The desktop app is never blocked by this work.

## 10. Risks and open points

- **GPU vendor on the server.** The code is vendor-agnostic (wgpu, self-test per device); only the Intel Arc 140V is *measured* on the cohort (Vulkan and DX12), NVIDIA and AMD are pending (`docs/gpu_vendor_report.md`). Measure on the server GPU at deployment (WP7), before clinical use, not before development.
- **WebGPU in hospital browsers.** Chrome / Edge ≥ 113 and Firefox ≥ 141 on Windows ship it; Safari 26 on macOS. Locked-down enterprise browser policies may disable it. The PNG QC path is the fallback (the headless preview renderers were removed 2026-10-16), and WP4 works without WebGPU at all.
- **Upload size over hospital networks.** A 0.5 mm CT is 100–500 MB; resumable chunks (WP3) and a visible progress bar are the mitigation, not a nicety.
- **`SharedArrayBuffer`.** Not used in v1. If the web viewer ever needs worker threads, the server must add COOP / COEP headers and every embedded resource must be same-origin or CORP-tagged.
- **Two approval kinds.** The method-log approval (QC gate, `checkreg.rs:75`) and the stage lock (`locks.rs`) stay distinct, as in the CLI; the client must show both states honestly (the `SubjectSummary` carries both).
- **Precomputed atlas meshes**: `.obj` committed, `.cache.json` not (D7); the cache key moves to a content hash in WP5.
- **Not in scope:** multi-tenant SaaS, mobile layouts. (Amended 2026-10-23: the `vat` stage, Lead Group cohort jobs and connectome maps are now in scope, §12; anything beyond the trusted network waits for authentication, WP6.)

## 11. Implementation status (2026-10-11)

WP1–WP5 were implemented against the Rust tree (uncommitted at the time of writing). Verified on Windows x86_64: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `wasm32-unknown-unknown` `cargo check` for the shared crates + `dbsloc-web`, and the full pytest suite (247 passed, 1 skipped after rebuilding the `dbsloc_reconstruct` extension). A live smoke test of `dbsloc_server` served `/api/v1/gpu` (real Intel Arc 140V, Vulkan, `GO`), `/datasets`, `/subjects`, `/subjects/{sid}`, and refused a `..` file key with 400.

| WP | Status | What exists | Verified | Deviations / not done |
| --- | --- | --- | --- | --- |
| **1** | **Done** | New crates **`dbsloc-protocol`** (wasm-safe `Stage`, `RunEvent`, `StageOutcome`, `RegBackend`, `DEFAULT_NORM_ISOTROPIC_MM`), **`dbsloc-api`** (DTOs), **`dbsloc-ui`** (guided-step + stage-settings state, `dbsloc-app` rewired onto it); `PipelineError::QcPending` + `RunEvent::StageAwaitingQc` (driver emits both, CLI tokens unchanged); `checkreg::apply_approval`; `subject_summary::subject_summary`; `run_pipeline_subprocess_with_cancel[_and_bin]` | `dbsloc-pipeline` 203 tests, `dbsloc-api`/`dbsloc-ui`/`dbsloc-app` tests; CLI pytest unchanged | `dbsloc-protocol` is a new crate (not in §2) — needed because `dbsloc-pipeline` is not wasm-safe. `apply_approval` in the pipeline is scalar; the viewer's per-modality variant is **not** unified. `StageAwaitingQc` carries the driver label (`normalize`), matching `StageFailed`. |
| **2** | **Done** | `dbsloc-server` (axum 0.8 + tokio): config, one pass-through auth-seam middleware, datasets / subjects / plan / files (allowlist + `Range`) / templates + atlas, SQLite queue (`rusqlite` bundled), one `dbsloc_pipeline_runner` subprocess per job, SSE replay-then-live, cancel, QC approval + resume, lock approval, `/gpu`, audit log | 54 unit + 5 integration tests (`tests/integration.rs`, fake runner — no GPU); live smoke test | Start-up **warns**, does not exit, when no GPU adapter (deviation from §8, so plan-only/test use works). Real GPU run through the API is **not** exercised here. 2026-10-11 review: file GET streams; handlers run in `spawn_blocking`; one active run per subject enforced in one transaction + partial unique index, subject ids canonicalised; awaiting_qc runs can be cancelled / rejected and always retire; SSE ends with `end`; graceful shutdown; `web_dir` serves the `dist/` at `/` with SPA fallback, optional `cors_origins` for development; tests in `tests/integration.rs` + `tests/runs_api.rs`. `plan` still reports `skip:approved` from the lock flag only (the driver's skip decision is not a pub function). |
| **3** | **Done** | Chunked resumable uploads + sha256, NIfTI header validation + `descrip`/`aux_file` scrub (gzip-bomb capped), BIDS placement + `rawimages.json` merge + rollback, `dcm2niix` convert path | unit + integration tests (commit, rollback, bad entity, gzip cap, converted-NIfTI commit) | `dcm2niix` v1.0.20260724 is **bundled** under `resources/dcm2niix/<platform>/` since 2026-10-14 (`DBSLOC_DCM2NIIX_BIN` → bundled → PATH); missing → `E_DCM2NIIX`. Placement and conversion moved to the shared `dbsloc_pipeline::import` module (2026-10-14). A section-4.2 review fix hardened `session`/`modality`/`suffix` against traversal. |
| **4** | **Done (skeleton)** | `dbsloc-web` (detached workspace, eframe `WebRunner`, wasm): sign-in, landing, upload (sha256 + chunked PUT), guided steps on `dbsloc-ui`, progress via SSE (reconnect-deduped), QC PNG approve/reject, review (reco + atlas bytes) | `cargo check --target wasm32-unknown-unknown` (+ `--tests`), native stub | **Detached from the root workspace** (`exclude = ["crates/dbsloc-web"]`) so native `cargo build --workspace` stays green. No `trunk` here → not built/served/run in a browser. 3D scene drawing is not wired (WP5 follow-up). 2026-10-11 review: GPU verdict read as `GO`/`BORDERLINE`; run tracking polls status and reconnects SSE; resume after QC follows `resumed_run`; scene keys from `/viz/scene`; uploads hash and send `Blob.slice` per chunk (no whole-file copy), DICOM goes through `/convert`; still not run in a browser. |
| **5** | **Partial** | `dbsloc-viz` `web` feature: byte loaders (`scene_volume::from_bytes`, `reco_json::load_mni_side_geometry_from_str/_bytes`, `atlas_wavefront` string parse, `web_atlas` precomputed AHEAD `.obj`), wasm gating of rayon/threads/blocking helpers, `checkreg_view` state module | native `scene-viewer`/`checkreg-viewer`/`web` builds, `cargo check --target wasm32-unknown-unknown` | Interactive checkreg **not** rewired onto `checkreg_view`; the browser 3D scene / WebGPU checkreg render is not drawn. `reco_json` was made `pub` for the web loaders. |

Runtime deps added by the server: `axum 0.8`, `tokio 1`, `tower-http 0.7`, `rusqlite 0.40` (`bundled`), `toml 1`, `tokio-stream`, `futures`, `nifti`, `flate2`. **WP6 (auth) and WP7 (deployment) are untouched**, so this is a trusted-network prototype only (`D2`): do not put patient data on it before WP6.

## 12. Lead Group / VTA / connectomes (2026-10-23, implemented)

Owner decision of 2026-10-23 (`specs/14_lead_group.md` §15 item 7): every Lead Group feature runs on the desktop and on the GPU server from the start (specs `07`, `14`, `15`). Implemented in `crates/dbsloc-server` (`stimulations.rs`, `groups.rs`, `connectomes.rs`, `multipart.rs`) and in the runner (`dbsloc_pipeline_runner`, group-job mode). Until authentication (WP6) lands, these features share the trusted-network-only rule of the rest of the server: no patient data and no clinical scores on a server outside it.

### 12.1 `vat` stage and stimulation settings

- **`vat` stage.** Runs inside the per-job `dbsloc_pipeline_runner` like every other stage (`specs/07` §7.1) with the same lock (`locks/vat.json`) and its outputs under `derivatives/leaddbs/sub-<id>/stimulations/<space>/<label>/`. The run request accepts `vat_backend` (`auto` / `wgpu` / `cpu`), `vat_labels` (valid labels, never `gs_…`; empty = every label) and `vat_stats_atlas` (an atlas set name, no path separators) beside the existing fields (`runs::RunRequestExt`, a flattened `RunRequest`: `dbsloc-api` has no `vat` fields yet, so the server reads them itself). A bad value is 400 `E_BAD_REQUEST`. The stage shares the GPU job slots with registration.
- **File allowlist.** `GET …/subjects/{sid}/files/{*key}` serves `stimulations/<space>/<label>/*.nii | *.nii.gz | *.json | *.mat`; any other file below `stimulations/` is 403 `E_BAD_KEY`, and so is anything below a `headmodel/` folder (the basis cache, `specs/07` §3.6) or a `.partial-*` work name. Keys must still belong to the subject (400 otherwise).
- **Settings endpoints** (`stimulations.rs`; the subject must resolve; the subject must not have a queued or running run, 409 `E_RUN_ACTIVE`; an `awaiting_qc` run does not block):

| Method | Path | Body → result |
| --- | --- | --- |
| `GET` | `/datasets/{ds}/subjects/{sid}/stimulations` | `[{label, space, state: "canonical" \| "importable"}]` from `vat::settings::list_labels` (`gs_*` labels are not listed) |
| `GET` | `…/stimulations/{label}` | the canonical JSON of `specs/07` §2.3 (`Stimulation`); 404 `E_NOT_FOUND` when absent, 422 `E_VAT_settings` for an invalid label |
| `PUT` | `…/stimulations/{label}` | `Stimulation` JSON (the body label must equal the path label, else 400 `E_VAT_settings`) → `normalise` + `check` (§2.6; failures are 422 with the code of the first failed rule and all messages joined) → `vat::settings::save` + `write_stimparameters_mat` (native `.mat` twin); never raw. A `gs_` label is refused with the rule-10 message. Response `{label, contacts_per_side, warnings, stimulation}`. Audit `stimulation_set` |
| `POST` | `…/stimulations/import` | multipart (`file` = `sub-<id>_desc-stimparameters.mat`, optional text field `label`) or the raw file with `?label=` → `read_stimparameters_mat` → the same commit as `PUT`. The temporary file lives in `incoming/` and is removed; `imported_from` records `{upload, sha256}`, no path. Audit `stimulation_import` |
| `POST` | `…/stimulations/{label}/preview` | `{stimulation?: Stimulation, backend?: "cpu" \| "auto"}` (default: the saved settings, `cpu`) → `vat::preview::preview` (§7.8, writes nothing) → `{label, warnings, sides: [{side, dims, affine, volume_mm3, raw_volume_mm3, binary_voxels, binary_runs: [[start, length]], e_mag: {index, value_v_per_m}, analytic, basis_cache, backend, seconds, touches_boundary, warnings}]}` on the output grid (flat x-fastest index; `e_mag` sparse, non-zero voxels only). One preview at a time (409 `E_PREVIEW_BUSY`); `auto` may use the GPU and is refused with 409 `E_GPU_BUSY` while a job runs. It runs in the server process (see 12.5) |

Limits: settings JSON 4 MiB, `.mat` upload 64 MiB. A stimulation cannot be named `import` (that path is the import route).

### 12.2 Lead Group endpoints

All under `/api/v1/datasets/{ds}/groups`; the group folder is `derivatives/leadgroup/<guid>/` (`guid`: letters and digits). Records and the twin go through the pipeline (`group::{create_group, load_record, save_record, write_m_twin, clinical::import_table}`); the server never edits them by hand.

| Method | Path | Body → result |
| --- | --- | --- |
| `POST` | `/groups` | `{guid?, title?, atlas_set?, dataset_name?}` → 201 `{guid, record, warnings}`; default guid = UTC `yyyymmddHHMMSS`; 409 `E_GROUP_EXISTS`; the record gets the dataset `d1` (`derivatives/leaddbs`); audit `group_create` |
| `GET` | `/groups` | `[{guid, title, dataset_name, patients, groups, clinical_variables, analyses, updated_at}]` |
| `GET` | `/groups/{guid}` | the record JSON (`dbsloc-group/1`) with dataset roots **relative**: `datasets[].root = ""`, `derivatives_root = "derivatives/leaddbs"` |
| `PUT` | `/groups/{guid}` | a record as `GET` returns it → `{record, warnings}`. The server forces every dataset onto the server's dataset (a `derivatives_root` that is absolute or has `..`, `:` or `\` is 400 `E_BAD_KEY`), makes patients `sub-<id>`, keeps the twin state, `analyses` and `created_at` it owns, refuses another `guid` or `dataset_name` (400) and a record that fails `validate` (422), then saves the record and rewrites the Lead Group twin. 409 `E_RUN_ACTIVE` while the group has a queued or running job. Audit `group_put` |
| `POST` | `/groups/{guid}/clinical` | TSV / CSV (multipart `file`, or the raw body; `?format=tsv\|csv`, `?replace=true`, `?subject_column=`) → `group::clinical::import_table` → `{imported, replaced, unknown_subjects, patients_without_row, warnings}`; at most 16 MiB; audit `clinical_upload` with the group id and the variable counts only, never a value |
| `GET` | `/groups/{guid}/files/{*key}` | a file below the group folder (record, twin, exports, analysis outputs; `Range` supported); `.partial-*` is 403; reading a key that contains `clinical` is audit-logged (`clinical_read`) |

**Twin with relative paths.** The record on disk keeps absolute roots (the pipeline needs them); the twin is built from a relative copy (`groups::save_record_and_twin`), so `patient.list` holds `derivatives/leaddbs/sub-<id>` (`specs/14` §4.6) and opens on a desktop inside `<dataset>/derivatives/leadgroup/<guid>/`. After a job the server rewrites the twin the same way. API output never carries a server path: records are relativised and the sanitizer middleware (tokens `<datasets>/…`, `<connectomes>`) covers messages; SSE events are sanitised before they are sent.

### 12.3 Group jobs

One job per cohort analysis, in the same SQLite table and on the same GPU slots as subject runs (`gpu_jobs`; FIFO); persisted, and re-queued or failed after a restart like any job. The job's subject is `@group:<guid>` (never a valid BIDS label), so one job per group is active at a time (409 `E_RUN_ACTIVE`) without touching the subjects' own limit. One `dbsloc_pipeline_runner` subprocess per job reads `{"group_job": {kind, group_file, options}}` as its first line instead of `RunOptions` (same `cancel` line and kill after a 10 s grace; exit 0 / 1 / 130) and writes ordinary `RunEvent` lines.

| Method | Path | Body → result |
| --- | --- | --- |
| `POST` | `/groups/{guid}/jobs` | `{kind, options}` → 202 `{id}`; `kind` ∈ `group_calc`, `group_stats`, `sweetspot`, `fibrefilter`, `netmap`, `export`; options are validated per kind and unknown keys are 400 `E_BAD_REQUEST`. Audit `group_job_submit` |
| `GET` | `/groups/{guid}/jobs/active` | the queued or running job's `RunStatus`, or `null` |
| `GET` | `/groups/{guid}/jobs/{id}` | `RunStatus` (`subject` = `@group:<guid>`; `stages_ok` / `stages_failed` / `stages_skipped` count subjects for `group_calc`, else one stage) |
| `GET` | `/groups/{guid}/jobs/{id}/events` | the same SSE as `/runs/{id}/events` (replay, `Last-Event-ID`, `end` event) |
| `POST` | `/groups/{guid}/jobs/{id}/cancel` | as run cancel; audit `group_job_cancel` |

(`/runs/{id}`, `/runs/{id}/events` and `/runs/{id}/cancel` also work for a group job id.) Options per kind, as the `dbsloc group …` flags: `group_calc` {`subjects`, `stop_on_first_failure`, `contact_distances`, `mirror`, `flip_transform` auto \| plain, `copy_settings_from`, `sync_settings` (boolean, needs `copy_settings_from`), `vat_backend`}; `group_stats` {`csv`, `lead_dbs`, `contact_distances`, `vat_backend`}; `sweetspot` {`variable`, `covariates`, `preset`, `settings` (partial `SweetspotSettings`, merged over the preset), `space`, `model`, `subjects`}; `fibrefilter` {`variable`, `covariates`, `connectome`, `space`, `model`, `settings` (partial `FibreSettings`), `scheme` (`CvScheme`, default leave-one-out), `seed`, `mirror`}; `netmap` {as `fibrefilter`, `settings` = partial `NetmapSettings`, `mask` grey \| brain \| all}; `export` {`only` twin \| clinical}. `connectome` is a catalogue id or an installed name, never a path; a mask file is not accepted on the server.

Events: `group_calc` emits `stage_started` / `stage_completed` (computed) / `stage_skipped` (up to date, cancelled) / `stage_failed` per subject in record order, then `pipeline_completed`; the other kinds emit one stage named after the kind (`log_line` for progress, `stage_completed` with the JSON summary of the outputs as group-relative file keys, or `stage_failed`). Outputs are fetched through `GET /groups/{guid}/files/{*key}`.

### 12.4 Connectomes

Config keys: `connectomes_dir` (default `<data_root>/connectomes`), `leaddbs_connectomes_dir` (read-only), `allow_connectome_download` (default `false`). The sanitizer token `<connectomes>` covers both roots; connectome files are in no allowlist and no route serves them (only derived per-subject maps and group outputs can be fetched). A group job gets the roots through the environment of its runner process.

| Method | Path | Body → result |
| --- | --- | --- |
| `GET` | `/connectomes` | `{download_allowed, transfer: <id> \| null, catalogue: [{id, name, class, kind, space, cohort, citation, acknowledgement, licence_name, licence_url, licence_text, licence_text_sha256, access, agreement_with, dataset_url, instructions, version, total_bytes, state: installed \| partial \| available, percent?, partial_bytes?}], installed: [{name, id, class, root, catalogue_id, space, version, missing}]}`; no paths |
| `POST` | `/connectomes/{id}/fetch` | `{accepted_by, licence_text_sha256, attest?}` → 202 `{id}`; 403 `E_CONN_DISABLED` unless `allow_connectome_download`; 404 unknown id; 400 empty `accepted_by`; 409 `E_CONN_TERMS` when the hash is not the current licence text or a `dua` entry lacks the attestation; 409 `E_CONN_BUSY` while another transfer runs (at most one per server). Runs `connectome_fetch::fetch_connectome` on a server thread (lock file, resume, polite rules, `accepted_via: "server"`); audit `connectome_fetch` with the id and `accepted_by` |
| `GET` | `/connectomes/{id}/fetch/events` | SSE of the `ConnectomeEvent`s of the current (or last) transfer (`started`, `file_started`, `progress`, `retry`, `verifying`, `extracting`, `done` with the folder name only), then an `end` event `{state: done \| failed \| cancelled, folder? \| code, message}` |
| `POST` | `/connectomes/{id}/fetch/cancel` | 204; the partial file stays for resume; transfers are also cancelled on server shutdown |

### 12.5 Not done

- `dbsloc-api` has no `vat` fields in `RunRequest` and no DTOs for the endpoints above; responses are `serde_json::Value` (settings use the pipeline's `Stimulation`). The web client does not call any of them.
- The stimulation preview runs in the server process (CPU by default), not in a runner subprocess, and takes no GPU slot; a GPU preview is refused while a job runs.
- Connectivity maps (`dbsloc connectome map` / `fibervals`, `specs/15` §9) have no endpoint or job kind yet.
- Group jobs are not given an atlas folder (`DBSLOC_LEADDBS_ATLASES_DIR` is not passed to the runner; the bundled atlases are used).
- A group stimulation (`gs_<guid>`) can be read (`GET …/stimulations/gs_<guid>`) but only a group job writes it.
- No authentication (WP6): the clinical files are readable by every client of the server.
