# Contributing to FastWarp

Thank you for considering a contribution. This page says how to ask for help, report a problem,
and set up, check and submit a change. By taking part you agree to the
[Code of Conduct](https://github.com/WyssCenter/fastwarp/blob/main/CODE_OF_CONDUCT.md).

## Maintainers and support

FastWarp is maintained by:

- Richard Köhler ([@richardkoehler](https://github.com/richardkoehler)), Wyss Center for Bio and
  Neuroengineering
- Jan Philipp Payonk ([@JPPayonk](https://github.com/JPPayonk)), University of Rostock

FastWarp is research software. Support is given on a best-effort basis through GitHub issues;
there is no guaranteed response time.

## Ask, report, get help

- Questions and usage problems: open a
  [GitHub issue](https://github.com/WyssCenter/fastwarp/issues).
- Bugs: open an issue with the bug report form. Give the FastWarp version, the operating
  system, the GPU and driver, the graphics API (Vulkan, Metal or DirectX 12), the output of
  `fastwarp.gpu_info()` and a minimal example that shows the problem.
- Feature requests: open an issue with the feature request form, and describe what you want to
  do before the solution you have in mind.
- Security problems: do not open a public issue; follow the
  [security policy](https://github.com/WyssCenter/fastwarp/blob/main/SECURITY.md).

## Development setup

You need:

- The Rust toolchain pinned in `rust-toolchain.toml`, with `clippy` and `rustfmt`. With
  [rustup](https://rustup.rs), `rustup toolchain install` in the repository installs it.
- [uv](https://docs.astral.sh/uv/) for the Python tools, which `uv.lock` pins (maturin, pytest,
  ruff, and ANTsPy in the `ants` group).
- [cargo-nextest](https://nexte.st) to run the Rust tests; the `fastwarp-niftiio` tests need one
  process per test.
- Python 3.14 for development (`.python-version`). ANTsPy has no Python 3.14 wheels yet, so work
  that uses the `ants` group sets `UV_PYTHON=3.13`, as CI does. The wheel supports Python 3.11
  and later.
- A GPU with a Vulkan, Metal or DirectX 12 driver. Without one, the GPU tests skip. On Linux,
  Mesa's lavapipe software driver (`mesa-vulkan-drivers`) works with
  `WGPU_FORCE_FALLBACK_ADAPTER=1` and `FASTWARP_GPU_ALLOW_SOFTWARE=1`, as in CI.

## Build, lint and test

These are the local equivalents of what CI runs (`.github/workflows/ci.yml`). CI builds and
installs a release wheel instead of `maturin develop`, and runs nextest with `--profile ci`; the
[testing guide](https://fastwarp.readthedocs.io/latest/testing.html) has the exact commands.

```bash
export UV_PYTHON=3.13
uv sync --locked --no-install-project --group ants
uv run --no-sync maturin develop --uv --release
uv run --no-sync pytest
PYO3_PYTHON="$PWD/.venv/bin/python" cargo nextest run --locked --workspace --features fastwarp-gpu/dev-tools
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --features fastwarp-gpu/dev-tools -- -D warnings
cargo clippy --locked -p fastwarp-gpu --all-targets -- -D warnings
uv run --no-sync ruff check python tests benchmarks scripts
uv run --no-sync ruff format --check python tests benchmarks scripts
```

On Windows, `PYO3_PYTHON` is the full path of `.venv/Scripts/python.exe`. Set
`FASTWARP_GPU_REQUIRE_ADAPTER=1` and `FASTWARP_REQUIRE_ANTSPY=1` to make a missing adapter or a
missing ANTsPy fail the tests instead of skipping them (CI does).

The tests that compare the engine with the ANTs programs are `#[ignore]`d. They run in
`full.yml` on non-draft pull requests. The
[testing guide](https://fastwarp.readthedocs.io/latest/testing.html) shows how to run them
locally, lists every test variable, and describes what each workflow runs.

## Documentation

The documentation in `docs/` is built with Sphinx and published on
[Read the Docs](https://fastwarp.readthedocs.io). Build it locally with warnings as errors, as
Read the Docs and the `docs.yml` workflow on pull requests do:

```bash
uv run --frozen --only-group docs sphinx-build -W --keep-going -b html docs docs/_build/html
```

Update the documentation in the same pull request when you change the Python API, an
environment variable, the recipe, or an output file.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/) with a lowercase imperative
subject, for example `fix: reject a mask with a different grid` or
`feat: add a cancel token to linear_session`.

## Pull requests

- Branch from `main` and keep the change focused.
- New behaviour comes with tests. A bug fix comes with a regression test that fails without
  the fix.
- Add an entry under `## Unreleased` in `CHANGELOG.md` for any change a user can see.
- CI must pass: lint, the Rust tests and the Python tests (`ci.yml`), and the documentation
  build (`docs.yml`) when the change touches the documentation. `full.yml` runs on non-draft
  pull requests.
- Every file keeps an SPDX header (`Apache-2.0`); data and binaries are covered by `REUSE.toml`.

## Code ported from ANTs or ITK

A file derived from [ANTs](https://github.com/ANTsX/ANTs) or [ITK](https://itk.org) keeps the
upstream copyright line, carries the Apache-2.0 header from the template in
[`docs/THIRD_PARTY_PORTS.md`](https://github.com/WyssCenter/fastwarp/blob/main/docs/THIRD_PARTY_PORTS.md),
states what changed, and gets a row in the table of that file (upstream file and version, class
or function, what changed). The upstream notices are in
[`NOTICE`](https://github.com/WyssCenter/fastwarp/blob/main/NOTICE). Do not copy code you have
no right to license under Apache-2.0.

## Licence of contributions

FastWarp is licensed under the
[Apache License 2.0](https://github.com/WyssCenter/fastwarp/blob/main/LICENSE). Unless you state
otherwise, a contribution you submit is licensed under Apache-2.0 as well (inbound = outbound,
Apache-2.0 section 5).

## Decisions and releases

The maintainers review and merge pull requests and decide which changes are accepted. They also
decide when to release; the steps are in
[`RELEASING.md`](https://github.com/WyssCenter/fastwarp/blob/main/RELEASING.md).
