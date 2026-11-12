# Installation

## Requirements

| Item | Requirement |
| --- | --- |
| Python | 3.11 or later. One `abi3` wheel per platform serves every Python from 3.11. |
| Python dependency | `numpy` 2.0 or later (the only runtime dependency in `pyproject.toml`). |
| GPU API | Metal on macOS, Vulkan on Linux and Windows, DX12 on Windows. |
| Optional | ANTsPy, to read images as `ANTsImage` and to compare with ANTs. |

Without a usable hardware GPU the registration functions raise `GpuUnavailableError`. See
[GPU checks and fallbacks](user-guide.md#gpu-checks-and-fallbacks).

TODO(author): state the minimum GPU driver versions, the minimum GPU memory for a typical
T1w + T2w registration, and the macOS, Windows and glibc versions you tested. The repository
only records the measurements in [Performance](performance.md) and that a 2 GB GPU is too small
for T1w + T2w at 1 mm.

## What is built and what is tested

`wheels.yml` builds one wheel per platform with Python 3.14 (maturin 1.16.0, `--release`):

| Wheel | Built on |
| --- | --- |
| Linux x86_64, manylinux 2.28 | `ubuntu-latest` |
| Linux aarch64, manylinux 2.28 | `ubuntu-24.04-arm` |
| macOS arm64 | `macos-latest` |
| macOS x86_64 | `macos-latest` (cross-built; no smoke test) |
| Windows x86_64 | `windows-latest` |

Each wheel except macOS x86_64 is installed and the Python tests run on its runner. The
workflow sets no software-GPU variable, so GPU tests run there only if the runner has an
adapter, and skip otherwise.

`ci.yml` runs the Rust and Python tests on Linux x86_64 with Python 3.13, on Mesa's lavapipe
software Vulkan driver. Hardware GPUs are not tested in CI. The hardware in
[Performance](performance.md) was measured by hand. The release workflow builds the wheels on Python 3.14 and runs their smoke
test on Python 3.11, the oldest supported version, with the locked NumPy and again with
NumPy 2.0.2 (`ci/numpy-floor.txt`). Python 3.12 is not tested separately.

## From PyPI

```bash
pip install fastwarp
```

## From a release wheel

The same wheels are attached to each GitHub release:

```bash
gh release download -R WyssCenter/fastwarp -p "*.whl" -D wheels
pip install --find-links wheels fastwarp
```

The first command downloads the wheels of the latest release and needs the
[GitHub CLI](https://cli.github.com/).

## From source

Building needs a Rust toolchain. `rust-toolchain.toml` pins version 1.99.0, which `rustup`
installs on first use.

```bash
pip install "git+https://github.com/WyssCenter/fastwarp.git"
```

The build backend is [maturin](https://www.maturin.rs) (`maturin>=1.9.4,<2`). For a development
checkout see [Testing](testing.md).

## Check the installation

```python
import fastwarp

print(fastwarp.__version__)
print(fastwarp.gpu_available())
print(fastwarp.gpu_info())
```

`gpu_info()` returns the adapter `name`, `backend`, `device_type`, `driver` and
`memory_budget_bytes`, or raises `GpuUnavailableError`.

## Environment variables

| Variable | Effect |
| --- | --- |
| `FASTWARP_GPU_BACKEND` | `vulkan`, `metal`, `dx12` or `auto` (default) |
| `FASTWARP_GPU_ADAPTER` | pick the adapter whose name contains this text |
| `FASTWARP_GPU_ALLOW_SOFTWARE=1` | accept a software adapter (lavapipe, WARP); slow, for tests |
| `FASTWARP_GPU_MEMORY_BUDGET_MB` | override the memory budget of the capacity check |
| `FASTWARP_GPU_WAIT_TIMEOUT_S` | seconds to wait for the GPU before failing (default 300) |
