# GPU and memory

FastWarp runs its registrations on the GPU through [wgpu](https://wgpu.rs), which talks to the
graphics driver through Vulkan, Metal or DirectX 12. This page explains which GPU it uses, how
much memory a registration needs, and how to fall back to ANTs when the GPU is missing or too
small.

## Requirements

- A hardware GPU with a driver for one of the {term}`graphics APIs <graphics API>` below.
  Discrete and integrated GPUs both work.
- Enough memory for the {term}`registration grid` (see [What fits](#what-fits)).
- Software adapters, which emulate a GPU on the CPU (Mesa's lavapipe, Microsoft WARP), are
  refused unless `FASTWARP_GPU_ALLOW_SOFTWARE=1` is set. They work but are slow; the tests use
  them on machines without a GPU.

FastWarp requires no optional GPU feature: it uses timestamp queries only when the GPU offers
them, and it adapts its buffer and workgroup limits to what the GPU reports
(`crates/fastwarp-gpu/src/device.rs`). When it opens a GPU it compiles its compute shaders and
checks them on a small known-answer test; a GPU that fails it is reported as unavailable. No minimum driver
version is defined yet.

FastWarp has run on these GPUs:

| GPU | System | Graphics API | Source |
| --- | --- | --- | --- |
| NVIDIA RTX A4000 | Linux | Vulkan | [Performance](../performance.md#earlier-measurements-on-non-public-data) |
| Apple M5 Pro | macOS (MacBook Pro) | Metal | [Performance](../performance.md#earlier-measurements-on-non-public-data) |
| Intel Lunar Lake, integrated | Linux (laptop) | Vulkan | [Performance](../performance.md#earlier-measurements-on-non-public-data) |
| AMD Radeon Pro 5500 XT | macOS (Intel iMac) | Metal | [Performance](../performance.md#earlier-measurements-on-non-public-data) |
| NVIDIA Quadro K620, 2 GB | Windows Server 2022 | Vulkan | [Performance](../performance.md#earlier-measurements-on-non-public-data) |
| Intel Arc 140V, integrated | Windows 11 (laptop) | Vulkan | the examples of this documentation |
| Mesa lavapipe (software) | Ubuntu, GitHub Actions | Vulkan | every CI run ([Testing](../testing.md#what-ci-runs)) |

No measurement on DirectX 12 is published. If {func}`~fastwarp.gpu_info` raises
{class}`~fastwarp.GpuUnavailableError` on a machine with a GPU, see
[Troubleshooting](../troubleshooting.md#no-gpu-is-found).

## Which GPU is used

| Operating system | Graphics APIs tried, in order |
| --- | --- |
| Linux | Vulkan |
| Windows | Vulkan, then DirectX 12 |
| macOS | Metal |

Among the adapters of an API, FastWarp prefers a discrete GPU, then an integrated one, then a
virtual one. If creating the device fails on one API, the next one in the list is tried.

To choose yourself:

- `FASTWARP_GPU_BACKEND` selects the API: `vulkan`, `dx12`, `metal` or `auto` (the default,
  the order above). An unknown value raises {class}`~fastwarp.GpuUnavailableError`.
- `FASTWARP_GPU_ADAPTER`, or the `adapter=` argument of every GPU function, selects the adapter
  whose name contains the given text, ignoring case. The argument takes precedence over the
  variable.

For example, on a machine with an Intel Arc GPU:

```python
import fastwarp

print(fastwarp.gpu_info(adapter="arc")["name"])
```

```{code-block} text
:class: fw-output
Intel(R) Arc(TM) 140V GPU (16GB)
```

With a name that matches no adapter, the call raises {class}`~fastwarp.GpuUnavailableError`.

All environment variables are listed under [Environment variables](../environment.md).

## The memory budget

Before a registration starts, FastWarp compares the memory it will need with a budget:

1. The graphics API reports how much device memory is free: the memory budget of
   `VK_EXT_memory_budget` on Vulkan (or, without that extension, the size of the device-local
   heap), the local video memory budget on DirectX 12, and the recommended working set on Metal.
2. On an integrated GPU, which shares its memory with the CPU (for example Apple silicon or
   Intel Lunar Lake), and on a software adapter, this figure is halved.
3. 1 GiB is subtracted and kept free for the desktop and other programs.

`FASTWARP_GPU_MEMORY_BUDGET_MB` replaces this budget with a number of mebibytes (MiB, 1024 × 1024
bytes); nothing is subtracted from it. When the API reports no memory figure and the variable is unset, the budget
is the device's largest buffer size, at most 4 GiB, minus 1 GiB, and FastWarp prints a note.

{func}`~fastwarp.gpu_info` reports the budget as `memory_budget_bytes`.

## What fits

The memory a {func}`~fastwarp.register_syn` run with a SyN stage needs depends on the number of
voxels of the registration grid ({func}`~fastwarp.iso_grid_dims`) and on the number of channels:

| Channels | Bytes per voxel of the registration grid |
| --- | --- |
| 1 | 168 |
| 2 | 188 |

These figures come from the engine's {term}`capacity check` (`crates/fastwarp-gpu/src/capacity.rs`),
which bounds the measured peak allocation of a SyN run. The check adds what the coarser SyN
levels leave allocated: the buffers of a level that are each under 1 MiB (the whole level on a
small grid, otherwise its images or only its reduction partials), all of a level that runs after
a larger one, and the last displacement field resampled from the previous level. It also adds
0.6 MB that the GPU's start-up check keeps. That adds about 16 % on a 56 × 64 × 48 grid and
under 2 % on a 1 mm head. 1 GiB is 1024³ bytes. If a moving image has more voxels than
the fixed image at the same spacing, each extra voxel adds 4 bytes per channel. In addition, the
largest single buffer, the displacement field at 12 bytes per voxel, must fit the device's limit
on one storage buffer.

The rigid and affine stages are checked on their own. They need the moving image (4 bytes per
voxel) and, for every level of every linear stage, the sample buffers of the
{term}`Mattes mutual information` metric, which grow with the voxels sampled at that level (25 %
by default), summed over the whole chain. That is much less than a SyN stage needs. A recipe
with both is checked phase by phase, because the linear buffers are freed before the SyN stage
starts, and is refused when the larger phase does not fit. So a
[stage-list recipe](recipes.md#stage-lists) without a SyN stage fits grids that the default
recipe does not.

For the 1 mm MNI template that ANTsPy ships (182 × 218 × 182 voxels), at three grid spacings:

| `iso_mm` | Grid | Voxels | 1 channel | 2 channels |
| --- | --- | --- | --- | --- |
| 2.0 | 91 × 109 × 91 | 0.9 million | 0.15 GiB | 0.17 GiB |
| 1.0 | 182 × 218 × 182 | 7.2 million | 1.15 GiB | 1.29 GiB |
| 0.5 | 363 × 435 × 363 | 57.3 million | 9.06 GiB | 10.12 GiB |

The `mni-0.5mm` size of [`fastwarp bench`](../cli.md#bench), 364 × 436 × 364 voxels, is a
synthetic volume with the MNI field of view, not this template's grid, so its memory need is
close to the last row but not equal to it.

With the 1 GiB reserve, a 1 mm registration of one channel on this template needs a discrete
GPU that reports a little over 2.1 GiB of free memory. The earlier measurements on the
[Performance](../performance.md#earlier-measurements-on-non-public-data) page used a different
template, MNI 2009b, on a 1 mm grid of 197 × 233 × 189 voxels (8.7 million): there, one channel
needs 1.38 GiB and two channels 1.54 GiB. On a 2 GB NVIDIA Quadro K620, the two-channel
registration was refused, and the one-channel one ran only with
`FASTWARP_GPU_MEMORY_BUDGET_MB=1900` (1.86 GiB).

## Check before you run

{func}`~fastwarp.fits_gpu` runs the capacity check of {func}`~fastwarp.register_syn` without
registering. Pass it the same `fixed` and `moving` images, the same `iso_mm` and the same
`recipe` (by default the default recipe); the number of channels follows from the number of
fixed images. It returns `False` when there is no usable GPU.

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))
for iso_mm in (2.0, 1.0, 0.5):
    print(iso_mm, fastwarp.fits_gpu(fixed, moving, iso_mm=iso_mm))
```

On the laptop of the other examples (Intel Arc 140V, memory budget 7.5 GiB) this printed:

```{code-block} text
:class: fw-output
2.0 True
1.0 True
0.5 False
```

With `recipe={"stages": [{"transform": "rigid"}, {"transform": "affine"}]}`, the same call
returned `True` at 0.5 mm as well (see [Stage lists](recipes.md#stage-lists)).

Without `moving`, {func}`~fastwarp.fits_gpu` assumes that the moving image has the fixed image's
grid, which undercounts when the moving image is larger; pass it for the exact answer. Arguments
that {func}`~fastwarp.register_syn` would refuse, such as three fixed images or a non-positive
`iso_mm`, raise `ValueError`.

A {class}`~fastwarp.LinearSession` has no capacity check: a session that does not fit fails with
{class}`~fastwarp.RegistrationError` when the GPU runs out of memory.

## Fall back to ANTs

{class}`~fastwarp.GpuUnavailableError` and {class}`~fastwarp.GpuCapacityError` are raised before
any work, so a pipeline can catch them and run ANTs instead:

```python
import fastwarp


def normalize(fixed, moving, out_prefix, run_ants):
    try:
        return fastwarp.register_syn(fixed, moving, out_prefix)
    except (fastwarp.GpuUnavailableError, fastwarp.GpuCapacityError) as error:
        print(f"GPU registration not possible ({error}); running ANTs")
        return run_ants(fixed, moving, out_prefix)
```

`run_ants` stands for your own ANTs call, for example {func}`ants.registration` or
`antsRegistration` with the [same schedule](recipes.md#the-same-schedule-in-ants). Other
failures raise {class}`~fastwarp.RegistrationError`; see [Errors](../errors.md).

## Waiting for the GPU

Each wait for submitted GPU work is limited to 300 seconds; a longer wait fails with
{class}`~fastwarp.RegistrationError`. `FASTWARP_GPU_WAIT_TIMEOUT_S` changes the limit, for
example on a slow or busy GPU.
