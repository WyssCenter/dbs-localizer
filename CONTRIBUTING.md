# Contributing to Wyss DBS Localizer

Thank you for helping. This guide explains how to report a problem, ask a question, build the software from source, run the tests and propose a change. If anything here is unclear, open an issue and ask. Everyone taking part in the project follows our [Code of Conduct](CODE_OF_CONDUCT.md).

- [Reporting problems and asking questions](#reporting-problems-and-asking-questions)
- [Before you write code: write from the specifications](#before-you-write-code-write-from-the-specifications)
- [Development setup](#development-setup)
- [Building and running from source](#building-and-running-from-source)
- [Running the tests](#running-the-tests)
- [Code style and checks](#code-style-and-checks)
- [Proposing a change](#proposing-a-change)
- [Where things are](#where-things-are)
- [Releases](#releases)

## Reporting problems and asking questions

Use the [issue tracker](https://github.com/WyssCenter/dbs-localizer/issues) for bug reports, feature requests and questions. For anything you do not want to discuss in public, write to the maintainer, Richard Köhler (<richard.koehler@wysscenter.ch>). A good bug report contains:

- the version (`dbsloc --version`) and your operating system;
- the graphics card and the verdict of `dbsloc gpu probe`;
- the steps that lead to the problem, what you expected and what happened instead;
- the error message, the output of `dbsloc run` or the session log of the application (the path is shown under `F1`).

**Never attach patient data** or a log that contains patient names, dates of birth or other identifying information. If a problem only shows with a particular dataset, describe the images (modality, voxel size, orientation, scanner) or share an anonymised copy privately with the maintainers.

## Before you write code: write from the specifications

Wyss DBS Localizer is written from written specifications, not from the Lead-DBS source code. Lead-DBS and this project are both licensed under the GPL-3, but the project keeps its code free of material derived from Lead-DBS, so that the Wyss Center holds the copyright of the code and the licence of each part stays clear. Therefore:

- **Do not read, copy or translate the Lead-DBS MATLAB source** when you work on code in `crates/`, `dbsloc/` or `tests/`.
- Base your work on the specifications in [`specs/`](specs), on published papers and on general knowledge of the algorithms.
- If the specification does not answer your question, open an issue. A maintainer who is allowed to study Lead-DBS's behaviour will extend the specification; you then implement from the updated text.
- Code adapted from ANTs or ITK (Apache-2.0) must keep its licence notice and may only go into an Apache-2.0 crate; see [`NOTICE`](NOTICE) and [`fastwarp/docs/THIRD_PARTY_PORTS.md`](fastwarp/docs/THIRD_PARTY_PORTS.md).
- Every new source file starts with the SPDX header of its directory (`Apache-2.0` for every file under `fastwarp/`; `GPL-3.0-only` elsewhere). New data or binaries get an entry in [`REUSE.toml`](REUSE.toml). Rules: [`docs/LICENSING.md`](docs/LICENSING.md); `tests/test_licensing.py` checks them.

Contributions that do not follow this rule cannot be accepted.

## Development setup

You need:

| Tool | Why | How to get it |
| --- | --- | --- |
| Git | get the source | [git-scm.com](https://git-scm.com) |
| Rust | build the application, the command-line tool and all libraries | install [rustup](https://rustup.rs); the exact Rust version is pinned in [`rust-toolchain.toml`](rust-toolchain.toml) and is installed automatically on the first build |
| C/C++ build tools | Rust needs a linker | Windows: *Visual Studio Build Tools* with the *Desktop development with C++* workload; Linux: `build-essential` (Debian / Ubuntu) or `base-devel` (Arch); macOS: `xcode-select --install` |
| Python 3.14 and [uv](https://docs.astral.sh/uv/) | Python tests, electrode-geometry generator, packaging and cohort scripts | install uv; `uv sync` fetches Python 3.14 and the development packages |
| A Vulkan, DirectX 12 or Metal driver | run the graphics-card code and its tests | a current graphics driver; on a Linux machine without a graphics card, Mesa's software driver (`sudo apt install mesa-vulkan-drivers`) |

Python is only needed for development. The application and `dbsloc` are pure Rust.

Get the source and set up the Python environment:

```bash
git clone https://github.com/WyssCenter/dbs-localizer.git
cd dbs-localizer
uv sync                      # Python 3.14 environment with pytest, ruff, ty, maturin and pre-commit
uv run pre-commit install    # run the formatters on every commit and the linters on every push
```

The repository contains binaries and data (ANTs, dcm2niix, the MNI template, atlases) under `resources/`. The source tree finds them on its own; no download is needed.

## Building and running from source

```bash
cargo build -p dbsloc-app --release     # desktop application: target/release/dbs-localizer
cargo build -p dbsloc-cli --release     # command-line tool:   target/release/dbsloc
```

On Windows the programs end in `.exe`. You can also build and start in one step:

```bash
cargo run -p dbsloc-app --release
cargo run -p dbsloc-cli --release -- run /path/to/bids-dataset 01
```

A release build is much faster than a debug build for real images; use debug builds for tests and quick changes.

Build profiles (`Cargo.toml`):

- `--release`: what we ship; use it for packaging and for runs whose timings or results you report.
- `--profile release-dev`: optimized like `--release` but compiled incrementally, so rebuilding after a small edit takes seconds instead of about a minute (output in `target/release-dev/`). The programs can run a little slower than a `--release` build. Build both programs with it (`-p dbsloc-cli -p dbsloc-app`): the command-line tool looks for the viewer in `DBSLOC_GUI_BIN`, then its own folder, then `PATH`, then `target/release` and `target/debug`, so a CLI built alone may start an installed or older viewer.
- the default debug profile keeps only line tables (enough for panic backtraces) and no debug information for dependencies; `--profile debugging` builds everything with full debug information for a debugger.

On Windows, `.cargo/config.toml` links with `rust-lld`, which ships with the Rust toolchain; it links large debug builds faster than the Microsoft linker. `scripts/package_release.py` overrides it with `link.exe`, so the shipped programs are linked as before. If your editor runs rust-analyzer, give it its own target directory (`rust-analyzer.cargo.targetDir = true`) so its checks do not wait for, or hold up, your builds.

Useful while developing:

- `dbsloc run <dataset> <subject> --plan` shows which steps would run without starting any of them.
- `dbsloc run ... --headless` does not open the review and 3D windows (for remote machines and scripts).
- `DBSLOC_APP_SCREENSHOT_PNG=<file>` makes the application render one frame to a PNG file and exit, which helps when you change the user interface.
- [`docs/RUN_REFERENCE.md`](docs/RUN_REFERENCE.md) lists every option and environment variable.

To build a self-contained archive like the release downloads:

```bash
uv run python scripts/package_release.py    # Windows: dist/dbsloc-win.zip; Linux / macOS: dist/dbsloc-<platform>.tar.gz
```

## Running the tests

Two GitHub Actions workflows run the tests. [`check.yml`](.github/workflows/check.yml) (lint, fast Rust and Python tests, about ten minutes) runs on every pull request update and every push to `main`. [`full.yml`](.github/workflows/full.yml) (bundled-ANTs and software-GPU tests, the end-to-end tests on a synthetic head and the macOS jobs) runs when a pull request is not a draft, when you mark it ready for review, and nightly if `main` changed. Open a pull request as a draft while you iterate, run the checks below before you push, and push in batches; documentation-only changes start no workflow. To run the tests yourself:

```bash
# Rust: the application crates, then the registration engine in fastwarp/ (its own workspace), including the graphics-card kernel tests
cargo test --workspace
cargo test --manifest-path fastwarp/Cargo.toml --target-dir target --workspace --exclude fastwarp-py --features fastwarp-gpu/dev-tools

# one crate, or one test by name
cargo test -p dbsloc-pipeline
cargo test -p dbsloc-pipeline manual_correction

# Python and end-to-end tests: they run the release build of dbsloc on a small synthetic head
cargo build -p dbsloc-cli --release
DBSLOC_BIN=target/release/dbsloc uv run pytest           # Windows PowerShell: $env:DBSLOC_BIN="target\release\dbsloc.exe"
uv run pytest -m "not slow"                             # skip the long end-to-end runs
```

Notes:

- [cargo-nextest](https://nexte.st) runs the Rust tests in parallel and is what CI uses: `cargo nextest run --workspace` and `cargo nextest run --manifest-path fastwarp/Cargo.toml --target-dir target --workspace --exclude fastwarp-py --features fastwarp-gpu/dev-tools`. Tests that call the real ANTs programs read the directory from `DBSLOC_ANTS_DIR` (for example `resources/ants/win`) and skip when it is unset.
- Without a graphics card, the GPU tests skip themselves. To run them on the CPU with a software driver, set `WGPU_FORCE_FALLBACK_ADAPTER=1` and `DBSLOC_GPU_ALLOW_SOFTWARE=1`.
- Some tests are marked `#[ignore]` (Rust) or `integration` (pytest) because they need the Lead-DBS tutorial dataset or a fast graphics card. They are not needed for a normal contribution.
- Some Python tests need the optional native module: `uv run maturin develop --release -m crates/dbsloc-pipeline/Cargo.toml`. Tests that need it skip when it is missing.

Every change in behaviour needs a test. A bug fix should come with a test that fails without the fix.

## Code style and checks

CI rejects changes that fail these checks; `pre-commit` runs the same ones locally.

```bash
cargo fmt --all
cargo fmt --manifest-path fastwarp/Cargo.toml --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --manifest-path fastwarp/Cargo.toml --target-dir target --workspace --all-targets --features fastwarp-gpu/dev-tools -- -D warnings
uv run ruff check dbsloc/ tests/ scripts/
uv run ruff format dbsloc/ tests/ scripts/
uv run ty check dbsloc/
uv run pre-commit run --hook-stage manual --all-files   # slow extra checks: pytest, cargo test and a dependency audit
```

Before you commit a lockfile (`Cargo.lock`, `uv.lock`) that adds or updates many packages, read the lockfile diff for new transitive dependencies and run the dependency audits that CI runs in [`audit.yml`](.github/workflows/audit.yml):

```bash
uv audit --frozen
cargo install cargo-deny --locked   # once
cargo deny --locked check advisories bans licenses sources
cargo deny --locked --manifest-path crates/dbsloc-web/Cargo.toml --config deny.toml check advisories bans licenses sources   # the browser client has its own lockfile
```

[`deny.toml`](deny.toml) allows crates.io only and an allow-list of permissive licenses. A new dependency under another license (in particular GPL-2.0-only, AGPL or a licence with further restrictions, which a GPL-3.0-only program cannot include; the Apache-2.0 engine in `fastwarp/` accepts permissive licences only) is a decision for a maintainer: open an issue instead of extending the list.

Further rules:

- Rust code follows [`docs/agent-rules/rust-safety.md`](docs/agent-rules/rust-safety.md); graphics code also [`docs/agent-rules/rust-gpu-perf.md`](docs/agent-rules/rust-gpu-perf.md); user-interface code [`docs/agent-rules/gui-ux.md`](docs/agent-rules/gui-ux.md) (tooltips, keyboard shortcuts, progress display, colours from [`docs/BRAND.md`](docs/BRAND.md)).
- Text that users see (labels, buttons, `--help`, error messages) is plain English without unexplained abbreviations: write *directory*, not *dir*.
- Add a dependency only when it is well maintained and clearly needed; prefer the standard library. See *Dependency Policy* in [`AGENTS.md`](AGENTS.md). A new Rust crate appears in the `THIRD_PARTY_LICENSES.md` bundle of the release packages automatically (`scripts/third_party_licenses.py`); nothing to edit by hand.

## Proposing a change

1. For anything larger than a small fix, open an issue first and describe what you want to change, so that we can agree on the approach before you spend time on it.
2. Create a branch from `main` (in a fork if you do not have write access).
3. Make the change with tests, and run the checks above.
4. Keep the documentation in step with the code:
   - behaviour that a specification fixes: update the matching file in [`specs/`](specs);
   - new or changed options: [`docs/RUN_REFERENCE.md`](docs/RUN_REFERENCE.md) and the `--help` text;
   - anything a user notices: a line under `## [Unreleased]` in [`CHANGELOG.md`](CHANGELOG.md) ([Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format, written for users);
   - module status: section 12 of [`AGENTS.md`](AGENTS.md).
5. Write commit messages in the [Conventional Commits](https://www.conventionalcommits.org) style, for example `fix(reconstruct): keep the roll of an accepted lead`.
6. Open a pull request against `main`. Describe what changed, why, and how you tested it. CI must pass, and a maintainer reviews every pull request before it is merged.

By submitting a pull request you confirm that you wrote the code yourself, that it follows the rule above (write from the specifications), and that you have the right to contribute it under the licence of the files you change (the SPDX header of each file; [`docs/LICENSING.md`](docs/LICENSING.md)).

## Where things are

| Path | Contents |
| --- | --- |
| `crates/dbsloc-pipeline` | the pipeline: dataset reading, the steps, locks, outputs; shared by every front end |
| `crates/dbsloc-app` | desktop application `dbs-localizer` |
| `crates/dbsloc-cli` | command-line tool `dbsloc` |
| `crates/dbsloc-ui` | logic shared by the user interfaces (settings, batch queue) |
| `crates/dbsloc-viz` | 3D viewer, registration review and image viewers (wgpu) |
| `fastwarp/` | registration engine (Apache-2.0), a git subtree of [WyssCenter/fastwarp](https://github.com/WyssCenter/fastwarp) with its own Cargo workspace: `crates/fastwarp-reg`, `crates/fastwarp-gpu` (registration algorithms and their graphics-card implementation), `fastwarp-core`, `fastwarp-niftiio`; change it only in a commit that touches nothing else, see [`docs/LICENSING.md`](docs/LICENSING.md) *The fastwarp subtree* |
| `crates/dbsloc-vat-gpu` | graphics-card E-field solver for the VTA stage |
| `crates/dbsloc-reconstruct` | electrode reconstruction (TRAC/CORE, PaCER) and orientation detection |
| `crates/dbsloc-vat` | electric-field and activated-tissue model |
| `crates/dbsloc-connectome`, `crates/dbsloc-reports` (and `fastwarp/crates/fastwarp-matfile`) | connectome readers, session-report import (and MATLAB file reading and writing) |
| `crates/dbsloc-server`, `crates/dbsloc-web`, `crates/dbsloc-api`, `crates/dbsloc-protocol` | experimental server and browser client (prototype, see [`docs/SERVER_CLIENT_PLAN.md`](docs/SERVER_CLIENT_PLAN.md)) |
| `dbsloc/` | Python development tooling (electrode catalogue, research helpers) |
| `tests/` | Python and end-to-end tests |
| `specs/` | specifications: what each step must do |
| `docs/` | reference documents, validation reports, release process |
| `resources/` | bundled programs and data: ANTs, dcm2niix, template, atlases, licences |
| `scripts/` | packaging, release and validation scripts |

[`AGENTS.md`](AGENTS.md) holds the project's working notes: decisions, module status and conventions for contributors and coding assistants.

## Releases

Releases are made by the maintainers from git tags; [`docs/RELEASING.md`](docs/RELEASING.md) describes the process.
