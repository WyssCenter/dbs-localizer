# Troubleshooting

## No GPU is found

{func}`~fastwarp.gpu_available` returns `False`, or {func}`~fastwarp.gpu_info` raises
{class}`~fastwarp.GpuUnavailableError`. Start by reading the error message:

```python
import fastwarp

try:
    print(fastwarp.gpu_info())
except fastwarp.GpuUnavailableError as error:
    print(error)
```

A message for a machine where the name filter matched no GPU looks like this:

```{code-block} text
:class: fw-output
no usable GPU adapter: Backends(VULKAN): 1 adapter(s), none eligible; Backends(DX12): 2 adapter(s), none eligible; adapter filter 'no-such-gpu'; software adapters not allowed (FASTWARP_GPU_ALLOW_SOFTWARE=1)
```

What the parts of the message mean:

`Backends(VULKAN): 0 adapter(s)`
: The graphics API found no GPU at all. Install or repair the driver (see below).

`1 adapter(s), none eligible`
: A GPU was found but excluded by a filter listed later in the message. Check `adapter=` and
  `FASTWARP_GPU_ADAPTER`.

`adapter filter '...'`
: A name filter was set. Compare it with the names {func}`~fastwarp.gpu_info` shows without the
  filter.

`software adapters not allowed`
: The only adapters are software ones, such as Mesa's lavapipe. Install a driver for the real
  GPU, or set `FASTWARP_GPU_ALLOW_SOFTWARE=1` for slow tests.

`FASTWARP_GPU_BACKEND: ... unknown graphics API`
: The variable has a value other than `vulkan`, `dx12`, `metal` or `auto`. Fix or unset it.

### Linux

FastWarp uses Vulkan on Linux. It needs the Vulkan loader (`libvulkan.so.1`) and a Vulkan driver
for the GPU, which registers itself with a JSON file in `/usr/share/vulkan/icd.d/` or
`/etc/vulkan/icd.d/` ([Vulkan loader documentation](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md)).
On Debian and Ubuntu:

- the loader is the package `libvulkan1`;
- AMD and Intel GPUs use Mesa's driver, the package `mesa-vulkan-drivers`;
- NVIDIA's proprietary driver brings its own Vulkan driver.

`vulkaninfo --summary`, from the package `vulkan-tools`, lists the GPUs Vulkan sees. If it shows
no GPU, FastWarp cannot see one either.

**Headless machines and SSH.** Vulkan compute needs no display or window system, so FastWarp runs
on a server without a screen and over SSH. The user needs permission to open the GPU's device
files; with Mesa drivers these are the render nodes `/dev/dri/renderD*`
([kernel documentation](https://docs.kernel.org/gpu/drm-uapi.html#render-nodes)), which most
distributions give to the group `render` or `video`.

**Docker.** A container needs the GPU's device files and its Vulkan driver. For NVIDIA GPUs with
the NVIDIA Container Toolkit, Vulkan needs the `graphics` driver capability, for example
`NVIDIA_DRIVER_CAPABILITIES=compute,graphics,utility`
([NVIDIA documentation](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/docker-specialized.html)),
and the container needs the Vulkan loader. FastWarp has not been tested in containers.

**WSL 2.** In the Windows Subsystem for Linux, Linux programs reach the GPU through a DirectX 12
layer ([Microsoft documentation](https://learn.microsoft.com/windows/wsl/tutorials/gpu-compute));
Vulkan there needs Mesa's Dozen driver, which translates Vulkan to DirectX 12
([Dozen source code](https://gitlab.freedesktop.org/mesa/mesa/-/tree/main/src/microsoft/vulkan)). FastWarp has not been tested
in WSL 2. Installing the Windows wheel and running FastWarp in Windows avoids the extra layer.

### Windows

FastWarp tries Vulkan first, then DirectX 12. The graphics drivers of NVIDIA, AMD and Intel
usually include both. If Vulkan fails, `FASTWARP_GPU_BACKEND=dx12` forces DirectX 12. Update the
graphics driver from the GPU maker's website if neither works.

### macOS

FastWarp uses Metal, which every Mac supported by the wheels (macOS 11 or later) has. A virtual
machine may offer no GPU, or a virtual one, to the guest system; FastWarp has not been tested in
virtual machines. Run it on the host.

## The standalone program does not start

These apply to the standalone `fastwarp` program from release 0.2.0
([Command line: install](cli.md#install)).

`fastwarp: command not found` right after installing
: The installer added its folder to PATH, but a terminal that was already open does not see the
  change. Open a new terminal (on Linux and macOS, `. ~/.profile` also works). With
  `--no-modify-path` (`-NoModifyPath`) the installer printed how to add the folder yourself.

macOS: "cannot be opened" or "cannot be verified"
: The program is not code-signed, and macOS blocks a program from an archive downloaded with a web
  browser. Remove the quarantine flag with `xattr -d com.apple.quarantine fastwarp`, or download
  with `curl`, `gh` or the installer, which are not flagged.

Windows: SmartScreen warns about `fastwarp.exe`
: The same reason: the program is not code-signed, and SmartScreen may warn about a file
  downloaded with a web browser. Downloads made with the installer, `curl` or `gh` are not
  flagged.

The installer refuses to replace `fastwarp`
: It does not replace a `fastwarp` it did not install itself, such as the one of
  `uv tool install fastwarp`. Remove that one first, or install elsewhere with `--dir`
  (`-Dir`); see [What the installer does](cli.md#what-the-installer-does).

## The registration does not fit

{func}`~fastwarp.register_syn` raises {class}`~fastwarp.GpuCapacityError`. For example, two
channels on a 1 mm grid of 197 × 233 × 189 voxels with `FASTWARP_GPU_MEMORY_BUDGET_MB=1024`:

```{code-block} text
:class: fw-output
the SyN stage on the registration grid [197, 233, 189] needs 1658214780 bytes of device memory, but the budget is 1073741824 bytes (at 1 mm)
```

The message gives the grid, the bytes it needs and the memory budget. Choose one of:

- **A coarser grid.** Each doubling of `iso_mm` divides the number of voxels, and so the memory,
  by about eight. `fastwarp.fits_gpu(fixed, moving, iso_mm=...)` tells which `iso_mm` fits;
  [What fits](guide/gpu.md#what-fits) gives the bytes per voxel.
- **One channel instead of two**, if a second channel is not needed.
- **No SyN stage**, if an affine result is enough: a
  [stage-list recipe](guide/recipes.md#stage-lists) with only rigid and affine stages needs much
  less GPU memory.
- **Another GPU,** with `adapter=`, when the machine has a larger one.
- **ANTs,** as in [Fall back to ANTs](guide/gpu.md#fall-back-to-ants).

The budget leaves 1 GiB for other programs. If you know that the GPU has more free memory than
the graphics API reports, `FASTWARP_GPU_MEMORY_BUDGET_MB` sets the budget yourself; a budget that
is too large makes the run fail later with {class}`~fastwarp.RegistrationError` when the GPU runs
out of memory.

## The GPU stops responding

Each wait for GPU work is limited to 300 seconds (`FASTWARP_GPU_WAIT_TIMEOUT_S`). A longer wait,
or a GPU that the driver resets, ends the run with {class}`~fastwarp.RegistrationError`; a reset
shows as `device lost` in the message. On Windows, the operating system resets a GPU that does
not respond within a few seconds
([Timeout detection and recovery](https://learn.microsoft.com/windows-hardware/drivers/display/timeout-detection-and-recovery)).
Other programs that use the GPU heavily at the same time make both more likely; close them, or
use a coarser grid.

## Results differ slightly from ANTs

FastWarp follows the ANTs algorithms but does not reproduce ANTs' numbers digit for digit:

- The GPU computes in 32-bit floating point; ANTs computes in 64-bit floating point by default.
- Sums over many voxels are added in a fixed order on the GPU and in a different order in ANTs.
- ANTs with more than one thread is not repeatable even with a fixed random seed, because its
  threads add their partial sums in any order ([Accuracy benchmark](benchmarks.md#recipe)).
  FastWarp gives the same result on every run on one GPU.

The tests check how close the results are; [Performance and validation](validation.md#what-the-tests-check-today)
lists the tolerances. The [accuracy benchmark](benchmarks.md) compares both tools on public
data; its results are not published yet.

## Something else

Open an [issue](https://github.com/WyssCenter/fastwarp/issues) with the FastWarp version, the
operating system, the output of `fastwarp.gpu_info()` (or the error message), and a short
example. See [Contributing](contributing.md#ask-report-get-help).
