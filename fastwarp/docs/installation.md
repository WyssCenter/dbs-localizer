# Installation

## Requirements

| Item | Requirement |
| --- | --- |
| Python | 3.11 or later. One `abi3` wheel per platform serves every Python version from 3.11 on. |
| NumPy | 2.0 or later. It is the only runtime dependency. |
| GPU | A hardware GPU with a Vulkan, Metal or DirectX 12 driver. See [GPU and memory](guide/gpu.md). |
| ANTsPy (optional) | To read and write images in Python and to pass them as `ANTsImage`. The Python functions read no images from disk; the [command line](cli.md) reads NIfTI files itself. |

The resampling functions {func}`~fastwarp.compose_field` and {func}`~fastwarp.apply_transforms`
run on the CPU and need no GPU. Every other function that registers images needs one.

## Platforms

FastWarp is published on [PyPI](https://pypi.org/project/fastwarp/) as wheels for these
platforms. The minimum operating system is the one the wheel tag states; the GPU API is the one
FastWarp tries by default.

| Platform | Wheel tag | Minimum OS | GPU API |
| --- | --- | --- | --- |
| Linux x86_64 | `manylinux_2_28_x86_64` | glibc 2.28 | Vulkan |
| Linux aarch64 | `manylinux_2_28_aarch64` | glibc 2.28 | Vulkan |
| macOS on Apple silicon | `macosx_11_0_arm64` | macOS 11 | Metal |
| macOS on Intel | `macosx_11_0_x86_64` | macOS 11 | Metal (wheel cross-built on an Apple silicon runner and not tested) |
| Windows x86_64 | `win_amd64` | | Vulkan, then DirectX 12 |

On other platforms, `pip` builds FastWarp from the source distribution, which needs a Rust
toolchain (see [From source](#from-source)). Which of these wheels are tested, and how, is
described under [Testing](testing.md#what-the-release-workflow-tests).

## From PyPI

::::{tab-set}
:sync-group: installer

:::{tab-item} pip
:sync: pip

```bash
pip install fastwarp
```
:::

:::{tab-item} uv
:sync: uv

In a uv project:

```bash
uv add fastwarp
```

In an existing environment:

```bash
uv pip install fastwarp
```
:::

::::

To work with ANTsPy images, install ANTsPy as well; its package name is `antspyx`:

::::{tab-set}
:sync-group: installer

:::{tab-item} pip
:sync: pip

```bash
pip install antspyx
```
:::

:::{tab-item} uv
:sync: uv

```bash
uv add antspyx
```
:::

::::

ANTsPy 0.6.3, the version this repository tests with, has wheels for Python 3.11 to 3.13 on
Linux x86_64, macOS (Apple silicon and Intel) and Windows. Elsewhere, for example on Linux
aarch64 or with Python 3.14, pip builds it from its source distribution, which needs a C++
compiler. FastWarp itself has wheels for every Python from 3.11 on.

## Command line

The command `fastwarp` comes with the Python package: `pip install fastwarp` installs it next to
the Python functions. With uv you can also install only the command, in an environment of its
own, or run it without installing it:

::::{tab-set}
:sync-group: installer

:::{tab-item} pip
:sync: pip

```bash
pip install fastwarp
fastwarp --help
```
:::

:::{tab-item} uv
:sync: uv

```bash
uv tool install fastwarp
fastwarp --help
```

Without installing:

```bash
uvx fastwarp --help
```
:::

::::

The command line needs release 0.2.0 or later (see the [changelog](changelog.md)); release
0.1.0 does not have it. To run the development version, install FastWarp
[from source](#from-source), or run the command from source without installing it:

```bash
uvx --from "git+https://github.com/WyssCenter/fastwarp.git" fastwarp --help
```

From release 0.2.0 the command line also comes as a standalone program that needs no Python, with
installer scripts for Linux, macOS and Windows; see [Command line: install](cli.md#install).

## From a GitHub release

Each [GitHub release](https://github.com/WyssCenter/fastwarp/releases) carries the same wheels,
the source distribution and a `SHA256SUMS` file. With the
[GitHub CLI](https://cli.github.com/):

```bash
gh release download -R WyssCenter/fastwarp -D wheels
cd wheels
sha256sum --check --ignore-missing SHA256SUMS
```

Then install the wheel for your platform from that folder:

::::{tab-set}
:sync-group: installer

:::{tab-item} pip
:sync: pip

```bash
pip install --no-index --find-links . fastwarp
```
:::

:::{tab-item} uv
:sync: uv

```bash
uv pip install --no-index --find-links . fastwarp
```
:::

::::

The first command downloads every file of the latest release. `--no-index` takes FastWarp from
that folder only; NumPy must already be installed, or drop `--no-index` to fetch it from PyPI.

## From source

Building needs the Rust toolchain that `rust-toolchain.toml` pins (version 1.99.0);
[rustup](https://rustup.rs) installs it on first use. The build backend is
[maturin](https://www.maturin.rs).

::::{tab-set}
:sync-group: installer

:::{tab-item} pip
:sync: pip

```bash
pip install "git+https://github.com/WyssCenter/fastwarp.git"
```
:::

:::{tab-item} uv
:sync: uv

```bash
uv pip install "git+https://github.com/WyssCenter/fastwarp.git"
```

Only the command line, installed or run once:

```bash
uv tool install "git+https://github.com/WyssCenter/fastwarp.git"
uvx --from "git+https://github.com/WyssCenter/fastwarp.git" fastwarp --help
```
:::

::::

For a development checkout with the tests, see [Contributing](contributing.md) and
[Testing](testing.md).

## Check the installation

```python
import fastwarp

print(fastwarp.__version__)
print(fastwarp.gpu_available())
print(fastwarp.gpu_info())
```

On a laptop with an Intel Arc 140V integrated GPU this prints:

```{code-block} text
:class: fw-output
0.1.0
True
{'name': 'Intel(R) Arc(TM) 140V GPU (16GB)', 'backend': 'Vulkan', 'device_type': 'IntegratedGpu', 'driver': 'Intel Corporation', 'memory_budget_bytes': 8049165516}
```

{func}`~fastwarp.gpu_info` names the GPU FastWarp would use, its graphics API, its type and
driver, and the memory budget of the capacity check. Without a usable GPU,
{func}`~fastwarp.gpu_available` returns `False` and {func}`~fastwarp.gpu_info` raises
{class}`~fastwarp.GpuUnavailableError` with the reason. [GPU and memory](guide/gpu.md) explains
how the GPU is chosen and how to choose another one, and
[Troubleshooting](troubleshooting.md#no-gpu-is-found) what to do when no GPU is found.
