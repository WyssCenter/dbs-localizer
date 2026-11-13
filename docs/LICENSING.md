# Licensing

Which licence applies to which part of the repository, why, and what to do when you add a file. Decisions of 2026-11-09 and 2026-11-10 (product owner; on 2026-11-10 the application changed from proprietary to GPL-3.0-only). This is a record of the repository's state, not legal advice; Wyss Center legal / technology transfer approves any distribution outside the Wyss Center.

The repository follows the [REUSE](https://reuse.software/spec-3.3/) conventions:

- every source file (`.rs`, `.wgsl`, `.py`, `.sh`, `.cmd`) starts with an SPDX header;
- files that cannot carry a header (data, images, binaries, Markdown, manifests) are covered by [`REUSE.toml`](../REUSE.toml), where the last matching annotation wins and a file's own header wins over `REUSE.toml`;
- every licence identifier used has its full text in [`LICENSES/`](../LICENSES/);
- `tests/test_licensing.py` checks all of this on every run of the fast test suite (no extra tool).

## Map

| Path | SPDX licence | Why |
| --- | --- | --- |
| `fastwarp/` (git subtree of [WyssCenter/fastwarp](https://github.com/WyssCenter/fastwarp); crates `fastwarp-core`, `fastwarp-niftiio`, `fastwarp-matfile`, `fastwarp-reg`, `fastwarp-gpu`, plus the PyO3 binding `fastwarp-py` and its Python package, which this repository does not build) | `Apache-2.0` | The registration engine (in-process replacement for `antsRegistration` and `N4BiasFieldCorrection`, CPU reference and wgpu GPU path) and the crates it needs to build. About 40 files are ports of ITK v5.4.4 / ANTs v2.6.5 (Apache-2.0, listed in [`fastwarp/docs/THIRD_PARTY_PORTS.md`](../fastwarp/docs/THIRD_PARTY_PORTS.md)); keeping the whole engine Apache-2.0 matches upstream and lets it be published on its own. Each crate has its own `LICENSE`; `fastwarp-reg` and `fastwarp-gpu` also have a `NOTICE`. |
| Everything else in `crates/`, `dbsloc/`, `scripts/`, `tests/`, `packaging/`, `docs/`, `specs/` | `GPL-3.0-only` | Application code and documentation of the Wyss Center, free software since 2026-11-10. Includes the GPU VAT / E-field solver (`crates/dbsloc-vat-gpu`, moved out of `fastwarp-gpu` so the Apache crate does not depend on application crates). |
| `crates/dbsloc-viz/assets/brand/wyss-center-icon*.png` | `GPL-3.0-only` | Wyss Center logo. The image files are under the GPL (decision 2026-11-10); the logo and the name stay trademarks (`TRADEMARKS.md`). The application tile (`dbs-loc-icon*`, `packaging/wix/logo-64.png`) is GPL-3.0-only as well. |
| `resources/ants/` | `Apache-2.0` (Windows runtime DLLs: `LicenseRef-Microsoft-VCRedist`) | ANTs v2.6.5 binaries. |
| `resources/dcm2niix/` | `LicenseRef-dcm2niix` (DLLs: `LicenseRef-Microsoft-VCRedist`) | dcm2niix binaries; BSD-style licence with codec notices. |
| `resources/atlases/leaddbs/` | `GPL-3.0-only AND LicenseRef-LeadDBS-Atlas-Terms` | Verbatim Lead-DBS atlas data (DISTAL Minimal). Data read at run time, never compiled into a program. Research-only until the atlas terms are settled (`NOTICE`, `ATLAS_DISTAL.md`). |
| `resources/atlases/ahead/structures_mni09b/` | `CC-BY-4.0` | AHEAD structure masks and the surface meshes computed from them. |
| `resources/templates/space/MNI152NLin2009bAsym/res-1mm/` (images) | `LicenseRef-MNI-ICBM2009` | ICBM 2009b templates, McGill permission notice. |
| `resources/licenses/connectomes/` | `LicenseRef-Third-Party-Terms` | Dataset owners' terms, reproduced only to show them. |
| `tests/fixtures/reports/synthetic/` (PDF reports, expected values) | `GPL-3.0-only` | Session reports with invented content, written by `scripts/make_synthetic_session_report.py`. Reports exported from vendor software are not in the repository (owner decision 2026-11-10); the tests that compare against them read a folder named by `DBSLOC_MEDTRONIC_FIXTURES` and are skipped without it. |
| `crates/dbsloc-viz/assets/fonts/` | `OFL-1.1` | IBM Plex Sans. |

Third-party notices and attributions: [`NOTICE`](../NOTICE).

## Lead-DBS and the GPL

Decision of 2026-11-10 (product owner): Wyss DBS Localizer is released as free software under the GNU General Public License, version 3 only (`GPL-3.0-only`). Lead-DBS is GPL-3 too (its `LICENSE.md` names version 3 without "or later", which is why this repository uses `GPL-3.0-only` and not `-or-later`: the narrower grant can be widened later by the copyright holder, the wider one cannot be taken back).

What the choice means:

- The Apache-2.0 engine in `fastwarp/` can be combined with GPL-3 code (Apache-2.0 is compatible with GPL version 3); the combined programs `dbs-localizer` and `dbsloc` are distributed under GPL-3.0-only, the engine on its own stays Apache-2.0. The engine must not depend on GPL code, or it could no longer be published under Apache-2.0.
- Dependencies must be compatible with GPL-3.0-only: permissive licences are; `GPL-2.0-only`, AGPL and licences with further restrictions are not. `deny.toml` keeps its permissive allow-list.
- Code written in Phase C from `specs/` is still an independent implementation (`AGENTS.md` §9). The rule stays: code that is not derived from Lead-DBS is the Wyss Center's own copyright, so the Wyss Center can decide about its licence, and the Lead-DBS bundle also contains material under non-commercial terms that are not GPL-compatible.
- The specifications in `specs/` were written from a study of the Lead-DBS source and behaviour. Owner decision 2026-11-12: no similarity check against Lead-DBS is run, because both projects are GPL-3 and a part derived from Lead-DBS could be used under the GPL; the *Lead-DBS* section of [`NOTICE`](../NOTICE) credits the Lead-DBS developers and states their copyright and licence for any such part.
- The atlas data copied from Lead-DBS keeps its own label `GPL-3.0-only AND LicenseRef-LeadDBS-Atlas-Terms`: the owners' request not to use the atlases commercially travels with the data.
- The Wyss Center name and logo are trademarks. Owner decision 2026-11-10: the two logo image files are licensed under GPL-3.0-only like the rest of the software, because the top-bar logo is compiled into `dbs-localizer` and a file that is part of the program cannot carry copying restrictions beyond the GPL. Only the trademark is reserved (GPL version 3, section 7(e): no trademark rights are granted); `TRADEMARKS.md` says what a modified version may and may not do with the name and logo.
- A distributed binary must come with the corresponding source (GPL section 6). Every release package names the public repository and the tag it was built from, `https://github.com/WyssCenter/dbs-localizer/tree/v<version>`: `README.txt` in the archive and in the installed folder (`scripts/package_release.py`, checked on the extracted copy by `check_readme_source`), the `copyright` file and `Homepage` of the `.deb` (`scripts/build_deb.py`), the help link of the Windows installer (`packaging/wix/Product.wxs`); tests `tests/test_source_offer.py`. Not yet checked on built packages.
- The application tile (`dbs-loc-icon*`, `packaging/wix/logo-64.png`) is GPL-3.0-only. Its artwork was made by a Wyss Center employee as part of their work (confirmed by the product owner, 2026-11-10), so the Wyss Center holds the rights.

## Publishing the repository

Owner decisions 2026-11-10 and 2026-11-11: <https://github.com/WyssCenter/dbs-localizer> is a **new repository without the history** of the repository in which the project was developed until then. That earlier history holds files that may not be redistributed (third-party documents and test files) and personal e-mail addresses; it is kept as an internal archive. The new repository starts with one commit made from a tagged export of the earlier one (only tracked files, so ignored and untracked files cannot leak), followed by the `fastwarp/` subtree added again from the public engine repository <https://github.com/WyssCenter/fastwarp> at the engine commit the earlier repository vendored; engine changes reach upstream before the export, so the new repository carries none of its own. It is published once the open items of `docs/PHASE_HISTORY.md` (entries 2026-11-10 (b) and (c)) are closed.

Reading and writing Lead-DBS file formats (`uiprefs.mat`, `atlas_index.mat`, the reconstruction `.mat`) and describing Lead-DBS behaviour in specs does not make code derived. If material ever has to be copied from Lead-DBS, it goes under `resources/` as data with its licence beside it, never into a crate.

## Rules for new files

Source files start with the header of their directory. Rust and WGSL:

```rust
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0
```

under `fastwarp/` (the five Apache crates), and `GPL-3.0-only` everywhere else. Python and shell use the same two lines with `#`, after a shebang line if there is one; `.cmd` files use `@REM`.

A file ported from ITK or ANTs keeps the upstream copyright and the Apache header of the "Header template for ported files" in [`fastwarp/docs/THIRD_PARTY_PORTS.md`](../fastwarp/docs/THIRD_PARTY_PORTS.md) and gets a row there. It may only live in an Apache-2.0 crate, that is under `fastwarp/`.

An Apache-2.0 crate must not depend on a GPL-3.0-only workspace crate (normal, build or dev dependencies, including target-specific tables); `tests/test_licensing.py` fails if it does.

New third-party data or binaries: add an annotation to `REUSE.toml`, the licence text to `LICENSES/` (`LicenseRef-<name>.txt` when SPDX has no identifier) and a section to `NOTICE`.

## Publishing the engine on its own

The crates already live in their own upstream repository, <https://github.com/WyssCenter/fastwarp> (see the next section). Before they are published to crates.io or announced outside the Wyss Center:

1. Wyss Center approval (copyright and the Apache-2.0 patent grant).
2. Set `publish` and `license` in each `Cargo.toml` as needed; `fastwarp/` already holds the `LICENSE` / `NOTICE` files, `docs/THIRD_PARTY_PORTS.md` and `LICENSES/Apache-2.0.txt`.
3. Confirm the origin of the test data under `fastwarp/crates/fastwarp-reg/tests/data/` and `fastwarp/crates/fastwarp-matfile/tests/data/` and record it in the `README.md` beside the files (today they say the origin is not recorded): the `uiprefs_*.mat` files look synthetic (fixed creation date 2024-01-01), the transform files (`ctest.h5`, `ms*.mat`, `init*.txt`) look like ITK / ANTs output of a Wyss run; neither origin is recorded yet. Also check that no test needs a GPL-3.0-only crate or subject data.
4. Do not use "ANTs" or "ITK" in the project name in a way that suggests endorsement; "ANTs-compatible" is fine.

## The fastwarp subtree

`fastwarp/` is the upstream repository <https://github.com/WyssCenter/fastwarp> vendored as a squashed git subtree. It is a Cargo workspace of its own with its own `Cargo.lock`, `deny.toml`, `rust-toolchain.toml`, `REUSE.toml` and `LICENSES/`; the root workspace lists it under `exclude` and reaches the five engine crates through path dependencies (`path = "../../fastwarp/crates/<name>"`). It also holds the Python binding (`crates/fastwarp-py`, `python/`, `pyproject.toml`, `uv.lock`), which upstream builds and tests; here clippy and fmt cover it with the engine, the root ruff hooks skip `fastwarp/`, and it is excluded from `cargo test` / `nextest` / `build` (`--exclude fastwarp-py`), because a PyO3 extension-module crate does not link as a test binary on every platform. Everything under `fastwarp/` is Apache-2.0 and must never depend on GPL-3.0-only code or on data of this repository.

Add the remote once per clone:

```
git remote add fastwarp https://github.com/WyssCenter/fastwarp.git
```

Take upstream changes:

```
git subtree pull --prefix=fastwarp fastwarp main --squash
```

Contribute a change back: make it in a commit that touches only `fastwarp/`. Nothing enforces this: `git subtree push` silently splits a mixed commit and copies its message unchanged, so check `git log --stat` before pushing. `git subtree push` copies the commit messages to the public repository, so the message must be fit for it (no patient data, no internal context). Then push to a branch and open a pull request in the fastwarp repository:

```
git subtree push --prefix=fastwarp fastwarp <branch>
```

Run the engine's own checks from the repository root (the shared `--target-dir` reuses the root build cache):

```
cargo test --manifest-path fastwarp/Cargo.toml --target-dir target --workspace --exclude fastwarp-py --features fastwarp-gpu/dev-tools
```

Tests that call the real ANTs programs read the directory from `DBSLOC_ANTS_DIR` (set it to `resources/ants/<platform>`; they skip when it is unset and fail when it is set but wrong); the DISTAL atlas test reads `DBSLOC_ATLAS_INDEX_MAT`. CI sets both (`check.yml`, `full.yml`); the ANTs tests are `#[ignore]`d and run only in the bundled-ANTs step of `full.yml`.

Dependencies and versions: the root `Cargo.lock` decides which third-party versions the application builds with and is covered by this repository's Dependabot and audit; `fastwarp/Cargo.lock` only serves the engine's standalone checks, is audited here (`audit.yml`) but updated upstream by fastwarp's own Dependabot and arrives with a subtree pull. The engine crates keep their own version (fastwarp's `[workspace.package]`), independent of the application version that `scripts/bump_version.py` sets.
