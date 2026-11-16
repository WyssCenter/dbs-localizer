# Testing

The tests live in two places: Rust tests in the engine crates and the Python binding
(`crates/*`), and Python tests in `tests/`. This page says what they cover, how to run them, and
what the continuous integration (CI) workflows run.

## Set up

Development uses Python 3.14 (`.python-version`). ANTsPy, which the comparisons with ANTs need,
has no Python 3.14 wheels yet, so work that uses the `ants` dependency group sets
`UV_PYTHON=3.13`, as CI does. Without it, a later `uv sync` rebuilds the environment on 3.14 and
drops ANTsPy.

```bash
export UV_PYTHON=3.13
uv sync --locked --no-install-project --group ants
uv run --no-sync maturin develop --uv --release
```

The last command builds the extension and installs it into `.venv` in editable form. You also
need the Rust toolchain of `rust-toolchain.toml` and [cargo-nextest](https://nexte.st); see
[Contributing](contributing.md#development-setup).

The lockfile covers every Python version the wheel supports, 3.11 and later.
`constraint-dependencies = ["scipy>=1.15"]` in `pyproject.toml` keeps the resolver from trading
SciPy down to 1.6 (no wheels, open advisories) for a newer NumPy; it can go once ANTsPy's
dependencies resolve to a current SciPy without it.

## Python tests

```bash
uv run --no-sync pytest -rs
```

`-rs` lists every skipped test with its reason. `tests/` holds:

| File | What it tests |
| --- | --- |
| `test_register.py` | {func}`~fastwarp.register_syn`: outputs, cancellation, capacity refusal, bad inputs, determinism, ANTs reading the transforms. |
| `test_recipe.py` | The recipe as data, checked before any work. |
| `test_rigid.py`, `test_linear.py` | Rigid and affine fits, masks, and agreement with ANTs fits. |
| `test_post.py` | {func}`~fastwarp.compose_field` and {func}`~fastwarp.apply_transforms` against ANTsPy. |
| `test_namespace.py` | The public names of the package. |
| `test_cli.py` | The [command line](cli.md): every command, its exit status, and that it gives the same results as the Python functions. With `FASTWARP_BIN` set to the standalone `fastwarp` program it also checks that the program behaves like `python -m fastwarp`; unset, those checks skip. |
| `test_benchmark_metrics.py`, `test_benchmark_fetch.py`, `test_benchmark_recipes.py` | The [accuracy benchmark](benchmarks.md): its metrics, hash-pinned downloads, safe extraction, recipes and pair selection, on tiny synthetic data (no GPU, no network). |
| `test_benchmark_smoke.py` | The accuracy benchmark end to end on a synthetic phantom with FastWarp and ANTs. Needs a GPU and ANTsPy; about 30 s on a laptop GPU. |

Details that matter when you change a test:

- Most tests of `test_register.py` need a GPU. The ones that compare with ANTs skip when ANTsPy
  is missing.
- `test_rigid.py`, `test_linear.py` and `test_post.py` start with
  `pytest.importorskip("ants")`, so without ANTsPy the whole module is skipped. In
  `test_post.py` the affine tests need no GPU; the composite tests do.
- `conftest.py` sets `ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS=1` before ITK starts in the test
  process (ITK reads it once). ITK must stay at one thread for the ANTs comparisons; a test that
  changes the variable restores it with `monkeypatch.setenv`.
- `test_linear.py` gives each ANTs fit a seed through `ANTS_RANDOM_SEED` (ANTsPy ignores a
  `random_seed` argument), so its ANTs fits give the same result on every run. The GPU's affine
  fit is compared with the per-corner median of five seeded ANTs fits, because single ANTs fits
  of the phantom scatter between 0.1 and 0.8 mm from the truth. The bounds were measured on
  Windows with an Intel Arc 140V; the values on the Linux CI (lavapipe) have not been measured.

## Rust tests

```bash
PYO3_PYTHON=.venv/bin/python cargo nextest run --locked --workspace --features fastwarp-gpu/dev-tools
cargo clippy --locked --workspace --all-targets --features fastwarp-gpu/dev-tools -- -D warnings
```

On Windows, `PYO3_PYTHON` is `.venv/Scripts/python.exe`. It is the Python the binding's Rust
tests are built against. The tests run under [cargo-nextest](https://nexte.st) because it runs
each test in its own process; the `fastwarp-niftiio` tests share scratch folders by name, which
races under the threads of `cargo test` (`.config/nextest.toml`).

## Tests that need a GPU

The GPU tests, Rust and Python, skip when no usable adapter opens (in Python,
`pytest.mark.skipif(not gpu_available())`), so the suite runs on a machine without a GPU. Two
variables turn skips into failures:

- `FASTWARP_GPU_REQUIRE_ADAPTER=1`: a missing adapter fails the tests.
- `FASTWARP_REQUIRE_ANTSPY=1`: an ANTsPy that does not import fails the tests.

On Linux without a GPU, Mesa's lavapipe software Vulkan driver runs the GPU tests, slowly:

```bash
sudo apt-get install -y mesa-vulkan-drivers
export WGPU_FORCE_FALLBACK_ADAPTER=1 FASTWARP_GPU_ALLOW_SOFTWARE=1 FASTWARP_GPU_REQUIRE_ADAPTER=1
```

## Tests against the ANTs programs

The Rust tests that compare the engine with `antsRegistration`, `antsApplyTransforms` and
`N4BiasFieldCorrection` are marked `#[ignore]`. They read the programs from the folder in
`FASTWARP_ANTS_DIR`, which must hold ANTs 2.6.5. Unset, the tests skip; a folder without a
program fails them.

```bash
FASTWARP_ANTS_DIR=/path/to/ants/bin cargo nextest run --locked -p fastwarp-reg --run-ignored only \
  -E 'binary_id(fastwarp-reg::ants_compat) | binary_id(fastwarp-reg::coreg_metric) | binary_id(fastwarp-reg::n4_ants)'
```

One more, `ants_apply_transforms_reads_the_registration` in `crates/fastwarp-cli/tests/cli.rs`,
registers a small phantom with `fastwarp register` and checks that `antsApplyTransforms` applies
the composite as `fastwarp apply` does. It also needs a GPU:

```bash
FASTWARP_ANTS_DIR=/path/to/ants/bin cargo nextest run --locked -p fastwarp-cli --run-ignored only \
  -E 'binary_id(fastwarp-cli::cli) & test(=ants_apply_transforms_reads_the_registration)'
```

All test variables are listed under [Environment variables](environment.md#tests-and-development).

## What CI runs

| Workflow | When | What |
| --- | --- | --- |
| `ci.yml` | Every push to `main` and every pull request | Lint (`cargo fmt`, `clippy`, `ruff`), the Rust tests and the Python tests on Linux x86_64 with Python 3.13, on lavapipe, with a missing adapter or ANTsPy counted as a failure. The Python tests also compare the standalone `fastwarp` program (`FASTWARP_BIN`) with `python -m fastwarp`. |
| `full.yml` | Non-draft pull requests that change more than documentation, nightly when `main` has changed, and by hand | The tests against the ANTs programs, with the three programs of the official ANTs 2.6.5 Ubuntu 24.04 release, checked against the SHA-256 sums in `.github/ants-2.6.5-ubuntu-24.04.sha256` and cached; the command-line test against `antsApplyTransforms` runs on lavapipe. |
| `docs.yml` | Pushes to `main` and pull requests that change the documentation or a file it reads, weekly, and by hand | The HTML build of the documentation with warnings as errors, as Read the Docs builds it (Python 3.13, `docs/requirements.txt` installed with `--require-hashes`). The HTML build fetches the intersphinx inventories of Python, NumPy and ANTsPy; the link check, which depends on every linked site, runs only weekly and by hand. |
| `audit.yml` | When a lockfile, manifest or workflow changes, and weekly | `cargo deny` and `uv audit`. |
| `wheels.yml` | A `v*` tag, or by hand (`gh workflow run wheels`) | Builds and tests the wheels; on a tag also publishes them (see below). |
| `paper.yml` | When `paper/` changes | Builds the draft paper as a PDF. |

`ci.yml` runs:

```bash
uv sync --locked --no-install-project --group ants
PYO3_PYTHON="$PWD/.venv/bin/python" cargo nextest run --locked --workspace \
  --features fastwarp-gpu/dev-tools --profile ci
```

in the job `rust` and, at the same time, in the job `python`:

```bash
uv sync --locked --no-install-project --group ants
uv run --no-sync maturin build --locked --release -o dist
uv pip install --no-deps dist/*.whl
cargo build --locked --release -p fastwarp-py -p fastwarp-cli --bin fastwarp
FASTWARP_BIN="$PWD/target/release/fastwarp" uv run --no-sync pytest tests -v -rs \
  -n auto --durations=20
```

The job `test` only reports whether both succeeded; it is the check that the ruleset of `main`
requires, together with `lint`.

The `ci` nextest profile (`.config/nextest.toml`) of the `rust` job keeps going after a failure,
prints the failures at the end, and starts two long GPU tests first.

The `python` job builds a release wheel, installs it, and runs `pytest` against the installed
wheel. The local commands above install the extension in editable form instead. The standalone
program is built with the features of the wheel (`-p fastwarp-py -p fastwarp-cli`), so that it
reuses what the wheel build compiled. `-n auto` runs the tests in one process per logical CPU
(pytest-xdist); a plain `pytest` runs them one after the other, which is the safer choice on a
hardware GPU, where every process opens the GPU. The tests that compare a run with the default
registration take it from the session fixture `baseline` (`tests/conftest.py`), which runs once
per process.

Hardware GPUs are not tested in CI: `ci.yml` runs the GPU tests on the lavapipe software
driver. The hardware in [Performance](performance.md) was measured by hand, and the examples of
this documentation were run on an Intel Arc 140V laptop GPU.

## What the release workflow tests

`wheels.yml` builds one wheel per platform with Python 3.14 and maturin 1.16.0 (`--release`):

| Wheel | Built on | Tested |
| --- | --- | --- |
| Linux x86_64, manylinux 2.28 | `ubuntu-latest` | yes |
| Linux aarch64, manylinux 2.28 | `ubuntu-24.04-arm` | yes |
| macOS arm64 | `macos-latest` | yes |
| macOS x86_64 | `macos-latest`, cross-built | no |
| Windows x86_64 | `windows-latest` | yes |

Each tested wheel is installed on its runner with Python 3.11, the oldest supported version, and
the Python tests run twice: with the locked NumPy and with NumPy 2.0.2 (`ci/numpy-floor.txt`),
the oldest supported NumPy. Python 3.12 is not tested separately. The workflow sets up no
software GPU, so the GPU tests run only where the runner offers a hardware adapter and skip
otherwise. Whether GitHub's `windows-latest`, `macos-latest` and Linux runners offer a hardware
GPU adapter to Vulkan or Metal is not documented in this repository; `pytest -rs` in the workflow
log lists the GPU tests that were skipped on each run.

## Building the documentation

The documentation is built with Sphinx without compiling the Rust extension: the API reference
is read from the source files `python/fastwarp/__init__.py`, `python/fastwarp/_api.py` and
`python/fastwarp/_core.pyi`.

```bash
uv run --frozen --only-group docs sphinx-build -W --keep-going -b html docs docs/_build/html
uv run --frozen --only-group docs sphinx-build -b linkcheck docs docs/_build/linkcheck
```

Use `--only-group docs`; `--group docs` would also build the project. Read the Docs builds with
Python 3.13 and installs `docs/requirements.txt`, a hash-pinned export of the lockfile, and treats
warnings as errors; `docs.yml` builds the same way on pull requests. Regenerate the file after
any change to the `docs` group or `uv.lock`:

```bash
uv export --frozen --only-group docs --no-emit-project --format requirements-txt -o docs/requirements.txt
```
