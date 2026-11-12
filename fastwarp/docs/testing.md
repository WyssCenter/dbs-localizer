# Testing

The tests live in two places: Rust tests in the engine crates and the binding (`crates/*`), and
Python tests in `tests/`.

## What CI runs and the local shortcut

`ci.yml` (every push to main and every pull request) runs, on Linux with Python 3.13:

```bash
uv sync --locked --no-install-project --group ants
PYO3_PYTHON="$PWD/.venv/bin/python" cargo nextest run --locked --workspace \
  --features fastwarp-gpu/dev-tools --profile ci
uv run --no-sync maturin build --locked --release -o dist
uv pip install --no-deps dist/*.whl
uv run --no-sync pytest tests -v -rs
```

CI builds a release wheel and installs it, then runs `pytest` against the installed wheel. The
local shortcut replaces the last three commands with `uv run --no-sync maturin develop --uv
--release && uv run --no-sync pytest`, which installs the extension in editable form and does
not use the `ci` nextest profile. The `ci` profile (`.config/nextest.toml`) keeps going after a
failure, prints failures at the end, and gives two long GPU tests a higher priority.

## Set up for local runs

Development uses Python 3.14 (`.python-version`). ANTsPy, which the comparisons with ANTs need,
has no Python 3.14 wheels yet, so work that uses the `ants` group sets `UV_PYTHON=3.13`, as CI
does. Without it a later `uv sync` rebuilds the environment on 3.14 and drops ANTsPy.

```bash
export UV_PYTHON=3.13
uv sync --locked --no-install-project --group ants
```

## Python tests

```bash
uv run --no-sync maturin develop --uv --release
uv run --no-sync pytest -rs
```

`-rs` lists every skipped test with its reason. `tests/` holds:

- `test_register.py`: `register_syn` (outputs, cancellation, capacity refusal, bad inputs,
  determinism, ANTs reading the transforms). Most tests need a GPU; the ones that compare with
  ANTs skip when ANTsPy is missing.
- `test_recipe.py`: the recipe as data, checked before any work.
- `test_rigid.py`, `test_linear.py`, `test_post.py`: `register_rigid`, affine fits and masks,
  `compose_field` and `apply_transforms`. Each starts with `pytest.importorskip("ants")`, so
  without ANTsPy the whole module is skipped, not just one test. In `test_post.py` the affine
  tests need no GPU; the composite tests do.

## Rust tests

```bash
PYO3_PYTHON=.venv/bin/python cargo nextest run --locked --workspace --features fastwarp-gpu/dev-tools
cargo clippy --locked --workspace --all-targets --features fastwarp-gpu/dev-tools -- -D warnings
```

On Windows use `.venv/Scripts/python.exe` for `PYO3_PYTHON`. It is the Python the binding's Rust
tests are built against. The tests run under [cargo-nextest](https://nexte.st) because each test
then runs in its own process; the `fastwarp-niftiio` tests share scratch folders by name, which
races under the threads of `cargo test` (`.config/nextest.toml`).

## Tests that skip without a GPU

The GPU tests, Rust and Python, skip when no usable adapter opens (`pytest.mark.skipif(not
gpu_available())` in Python), so that the suite runs on a machine without a GPU.
`FASTWARP_GPU_REQUIRE_ADAPTER=1` makes a missing adapter a failure instead. `FASTWARP_REQUIRE_ANTSPY=1`
does the same for an ANTsPy that does not import.

## Software GPU runs

On Linux, `ci.yml` installs Mesa's lavapipe software Vulkan driver and sets
`WGPU_FORCE_FALLBACK_ADAPTER=1`, `FASTWARP_GPU_ALLOW_SOFTWARE=1`, `FASTWARP_GPU_REQUIRE_ADAPTER=1` and
`FASTWARP_REQUIRE_ANTSPY=1`:

```bash
sudo apt-get install -y mesa-vulkan-drivers
export WGPU_FORCE_FALLBACK_ADAPTER=1 FASTWARP_GPU_ALLOW_SOFTWARE=1 FASTWARP_GPU_REQUIRE_ADAPTER=1
```

On Windows and macOS, no workflow sets up a software adapter. `wheels.yml` runs the Python
tests on `windows-latest`, `macos-latest` (arm64) and the Linux runners after installing the
wheel, with none of these variables, so GPU tests run only if the runner exposes a hardware
adapter and skip otherwise. The software adapter names in the source are lavapipe and WARP
(`FASTWARP_GPU_ALLOW_SOFTWARE`). TODO(author): say whether the Windows and macOS runners ran the
GPU tests or skipped them; the workflow files do not record it.

## Tests against the real ANTs programs

The Rust tests that compare the engine with `antsRegistration`, `antsApplyTransforms` and
`N4BiasFieldCorrection` are `#[ignore]`d. Set `FASTWARP_ANTS_DIR` to the folder that holds the
three programs of ANTs 2.6.5. Unset, the tests skip; a folder without a program fails them.

```bash
FASTWARP_ANTS_DIR=/path/to/ants/bin cargo nextest run --locked -p fastwarp-reg --run-ignored only \
  -E 'binary_id(fastwarp-reg::ants_compat) | binary_id(fastwarp-reg::coreg_metric) | binary_id(fastwarp-reg::n4_ants)'
```

`full.yml` runs them on non-draft pull requests, nightly when main moved and by hand, with the
programs from the official ANTs 2.6.5 Ubuntu 24.04 release, checked against the SHA256 sums in
`.github/ants-2.6.5-ubuntu-24.04.sha256`.

## Test variables

| Variable | Effect |
| --- | --- |
| `FASTWARP_GPU_REQUIRE_ADAPTER=1` | a missing GPU adapter fails the Rust and Python GPU tests instead of skipping them |
| `FASTWARP_REQUIRE_ANTSPY=1` | ANTsPy failing to import fails the Python tests instead of skipping the comparisons with ANTs |
| `WGPU_FORCE_FALLBACK_ADAPTER=1` | select software adapters only (with `FASTWARP_GPU_ALLOW_SOFTWARE=1`) |
| `FASTWARP_ANTS_DIR` | folder with the ANTs 2.6.5 programs for the `#[ignore]`d comparison tests |
| `FASTWARP_ATLAS_INDEX_MAT` | path to an atlas index `.mat` for a `fastwarp-matfile` test; unset skips it, a missing file fails it |
| `FASTWARP_UIPREFS_DIR` | folder of real `.mat` files for an `#[ignore]`d `fastwarp-matfile` test |
| `FASTWARP_REG_AB_DIR` | folder of a real subject (ANTs outputs) for the `#[ignore]`d A/B tests in `fastwarp-gpu` |
| `FASTWARP_REG_PROTO_DIR` | folder of prototype composite transforms for a `fastwarp-reg` test; unset skips it |
| `FASTWARP_REG_ANTS_COMPOSITE` | path of a real ANTs composite `.h5` for a `fastwarp-reg` test; unset skips it |
| `FASTWARP_GPU_BENCH` | enables the `#[ignore]`d GPU benchmarks in `syn_bench` |

## Other checks

`audit.yml` runs `cargo deny` and `uv audit` when a lockfile or manifest changes and weekly.
`wheels.yml` builds all wheels on a `v*` tag or by hand (`gh workflow run wheels`).

## Building the docs

The documentation is built with Sphinx on Python 3.13, without compiling the Rust extension.
The API reference is generated statically from `python/fastwarp/__init__.py` and
`python/fastwarp/_core.pyi`.

```bash
uv run --frozen --only-group docs sphinx-build -W --keep-going -b html docs docs/_build
```

Use `--only-group docs`; `--group docs` would also build the project. Read the Docs installs
from `docs/requirements.txt`, a hash-pinned export of the lock. Regenerate it after any change
to the `docs` group or `uv.lock`:

```bash
uv export --frozen --only-group docs --no-emit-project --format requirements-txt -o docs/requirements.txt
```
