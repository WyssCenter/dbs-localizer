# Contributing to FastWarp

Thank you for considering a contribution. This page says how to set up, check and submit a
change. By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ask, report, get help

- Questions and usage problems: open a [GitHub issue](https://github.com/WyssCenter/fastwarp/issues).
- Bugs: open an issue with the bug report form. Give the FastWarp version, the operating
  system, the GPU and driver, the backend (Vulkan, Metal or DX12), the output of
  `fastwarp.gpu_info()` and a minimal reproduction.
- Feature requests: open an issue with the feature request form and describe the use case
  before the solution.
- Security problems: do not open a public issue; follow [SECURITY.md](SECURITY.md).

Maintainers and response time: TODO(author): name the maintainers and the expected response
time (for example "one maintainer, a first reply within N working days").

## Development setup

You need:

- The Rust toolchain pinned in `rust-toolchain.toml` (with `clippy` and `rustfmt`). With
  [rustup](https://rustup.rs), `rustup toolchain install` in the repository installs it.
- [uv](https://docs.astral.sh/uv/) for the Python tools, which `uv.lock` pins (maturin, pytest,
  ruff, and ANTsPy in the `ants` group).
- [cargo-nextest](https://nexte.st) to run the Rust tests; the copied `fastwarp-niftiio` tests
  need one process per test.
- Python 3.14 for development (`.python-version`). ANTsPy has no 3.14 wheels yet, so work that
  uses the `ants` group sets `UV_PYTHON=3.13`, as CI does. The wheel supports Python 3.11 and
  later.
- A GPU with a Vulkan, Metal or DX12 driver. Without one, the GPU tests skip. On Linux, Mesa's
  `lavapipe` software driver (`mesa-vulkan-drivers`) works with `WGPU_FORCE_FALLBACK_ADAPTER=1`
  and `FASTWARP_GPU_ALLOW_SOFTWARE=1`, as in CI.

## Build, lint and test

These are the commands CI runs (`.github/workflows/ci.yml`; the README *Development* section
has the details):

```bash
export UV_PYTHON=3.13
uv sync --locked --no-install-project --group ants
uv run --no-sync maturin develop --uv --release
uv run --no-sync pytest
PYO3_PYTHON="$PWD/.venv/bin/python" cargo nextest run --locked --workspace --features fastwarp-gpu/dev-tools
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --features fastwarp-gpu/dev-tools -- -D warnings
cargo clippy --locked -p fastwarp-gpu --all-targets -- -D warnings
uv run --no-sync ruff check python tests
uv run --no-sync ruff format --check python tests
```

On Windows, `PYO3_PYTHON` is the full path of `.venv/Scripts/python.exe`. Set `FASTWARP_GPU_REQUIRE_ADAPTER=1` and
`FASTWARP_REQUIRE_ANTSPY=1` to make a missing adapter or a missing ANTsPy fail the tests instead of
skipping them (CI does).

The tests that compare the engine with the real ANTs programs are `#[ignore]`d. They run in
`full.yml` on non-draft pull requests. To run them locally, use the `FASTWARP_ANTS_DIR` command in
the README.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/) with a lowercase imperative
subject, for example `fix: reject a mask with a different grid` or
`feat: add a cancel token to linear_session`.

## Pull requests

- Branch from `main` and keep the change focused.
- New behaviour comes with tests. A bug fix comes with a regression test that fails without
  the fix.
- Add an entry under `## Unreleased` in `CHANGELOG.md` for any change a user can see.
- Update the README when you change the Python API, the environment variables or the recipe.
- CI must be green: lint, the Rust tests and the Python tests (`ci.yml`). `full.yml` runs on
  non-draft pull requests.
- Every file keeps an SPDX header (`Apache-2.0`); data and binaries are covered by `REUSE.toml`.

## Code ported from ANTs or ITK

A file derived from ANTs or ITK keeps the upstream copyright line, carries the Apache-2.0
header from the template in [`docs/THIRD_PARTY_PORTS.md`](docs/THIRD_PARTY_PORTS.md), states
what changed, and gets a row in the table of that file (upstream file and version, class or
function, what changed). The upstream notices are in [`NOTICE`](NOTICE). Do not copy code you
have no right to license under Apache-2.0.

## Licence of contributions

FastWarp is licensed under [Apache-2.0](LICENSE). Unless you state otherwise, a contribution
you submit is licensed under Apache-2.0 as well (inbound = outbound, Apache-2.0 section 5).

## Governance and support

TODO(author): state who decides on changes and releases, and what support users can expect.
FastWarp is research software; support is by best effort through GitHub issues.
