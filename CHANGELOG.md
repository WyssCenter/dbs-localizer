# Changelog

## Unreleased

- README badges: CI, PyPI version, Python versions, licence, documentation.

## 0.1.0 (2026-11-12)

- First public release, as `fastwarp`. The version restarts at 0.1.0; the internal
  versions before it are listed under *Development versions*.
- `NOTICE` and `crates/fastwarp-reg/NOTICE` reproduce the BSD-3-Clause notice of the Mersenne
  Twister code from which the sample-point generator derives through ITK.
- Python 3.11 is the oldest supported version (was 3.10, which has reached its end of life); the
  wheels are `abi3` wheels for Python 3.11 and later, and their smoke test runs on Python 3.11.
  The package declares `numpy>=2.0`, so NumPy 1.x is no longer accepted; the smoke test also runs
  on NumPy 2.0.2, the oldest supported version.
- Renamed from `dbsloc-gpureg` to `fastwarp`: the package and the module are `fastwarp`
  (`import fastwarp`), the binding crate is `crates/fastwarp-py`, and the repository is
  https://github.com/WyssCenter/fastwarp. The engine crates are now `fastwarp-core`, `fastwarp-niftiio`,
  `fastwarp-matfile`, `fastwarp-reg` and `fastwarp-gpu` (were `dbsloc-*`), and the developer
  binaries are `fastwarp_gpu_probe`, `fastwarp_reg_syn` and `fastwarp_reg_ab`. Code that imports
  `dbsloc_gpureg` must import `fastwarp` instead. The Rust crate names and the environment variable names changed;
  the Python API is unchanged apart from the import name.
- The environment variables are `FASTWARP_*` (`FASTWARP_GPU_BACKEND`, `FASTWARP_GPU_ADAPTER`,
  `FASTWARP_ANTS_DIR`, ...). The former `DBSLOC_*` names are still read when the new name is
  unset; a new name set to an empty value takes precedence over the old one.
- Licensed under Apache-2.0 (`LICENSE`); the wheel and the crates declare the licence and ship
  `LICENSE` and `NOTICE`.
- The `Private :: Do Not Upload` classifier is gone, so the wheels can go to PyPI.
- README timings measured on five GPUs (NVIDIA RTX A4000, Apple M5 Pro, Intel Lunar Lake, AMD
  Radeon Pro 5500 XT, NVIDIA Quadro K620 on Windows).
- Engine updated to revision `90c1df3` of its earlier standalone copy: wgpu 30, ndarray 0.17, nifti 0.18; the
  `fastwarp-matfile` tests carry their own data; the ANTs comparison tests read the programs from
  `FASTWARP_ANTS_DIR`. `ENGINE_SOURCE` is `"fastwarp-legacy 90c1df3"`: the revision belongs to
  the earlier standalone copy of the engine, not to this repository.
- Licence labels in REUSE form: an SPDX header in every source file, `REUSE.toml` for the rest,
  `LICENSES/Apache-2.0.txt`; `NOTICE` carries the ITK notice; `THIRD_PARTY_PORTS.md` moved to
  `docs/`.
- Development on Python 3.14 (`.python-version`); the CI tests run on 3.13, the newest Python
  ANTsPy ships wheels for. The lock covers every supported Python.
- The PyO3 `extension-module` feature is gone (deprecated in PyO3 0.30; maturin >= 1.9.4 sets
  `PYO3_BUILD_EXTENSION_MODULE` itself), so the binding's Rust unit tests link on Linux and run
  in CI. `wheels.yml` pins maturin 1.16.0.
- The idle Vulkan instance that keeps NVIDIA's Linux driver loaded lives in the engine
  (`fastwarp_gpu::device::new_instance`) instead of the binding, so every user of the engine gets
  it, not only the Python package; it is created only when Vulkan is among the requested APIs.
- The HDF5 transform reader rejects crafted files that could exhaust memory, take exponential or
  quadratic time, or panic, with an error instead of aborting the process: datatypes of size 0
  or of an unsupported class, chunk dimension sizes above 8 bytes, link names or filter
  parameters that overrun their message, chunk indices that list more chunks than the dataset
  has or run past the end of the file, chunk B-tree nodes and object
  header continuation blocks referenced twice, B-tree leaves, continuation blocks or stored
  chunks that together are larger than the file, and fixed array entries smaller than an
  address. The dataset size cap is checked before the chunk index is walked, chunks outside the
  dataset are skipped before they are decompressed, the shuffle element size is capped at the
  chunk length, and the output buffers are allocated fallibly.
- The GPU Mattes mutual-information metric of the linear stages reads its derivative partials
  through a staging buffer kept with the level instead of creating and dropping one on every
  derivative evaluation. Results are unchanged.
- Less host work per GPU SyN iteration, with bit-identical results: a displacement-field
  inversion creates its five bind groups once instead of four per inversion step (20 instead of
  320 per iteration), and the iteration waits for the GPU fewer times to read scalars back (4
  instead of 7 with one channel, 5 instead of 12 with two). The `Backend` trait gains
  `cc_value_derivative_pair_with_norms`, `max_scaled_norm_pair` and `invert_pair`, with default
  implementations that call the single operations. With two channels, the buffer pool is no
  longer trimmed at every iteration, only once per level, so very large grids stop freeing and
  recreating their reduction buffers every iteration.
- The GPU capacity check before `register_syn` counts the moving image on its own grid: a moving
  image with more voxels than the fixed one adds its smoothed copies to the planned memory, and
  can be the largest storage binding (`plan_capacity_pair`; `plan_capacity` is unchanged for
  images of equal size). On Vulkan the memory budget starts from the driver's live budget of the
  device-local heap minus what the process already uses (`VK_EXT_memory_budget`,
  `MemoryFigure::VulkanHeapBudget`) instead of the heap size, when the device reports one. On
  Metal it starts from the device's `recommendedMaxWorkingSetSize` minus its
  `currentAllocatedSize` (`MemoryFigure::MetalRecommendedWorkingSet`) instead of the fallback of
  at most 4 GiB.

## Development versions

Internal builds of the engine under the name `dbsloc-gpureg`, before the rename and the
Apache-2.0 licence. They were never published, and their tags and GitHub releases were
removed.

### dbsloc-gpureg 0.4.0 (2026-11-09)

- `linear_session` / `LinearSession` (was `rigid_session` / `RigidSession`, still available under
  those names): a fit can be an `Affine` stage (`register(transform="affine")`), started as the
  identity centred on `initial`, as ANTs centres a stage after its initial transform, and can
  bring its own fixed mask (`register(mask=...)`), for a template alignment (rigid, then affine)
  and a brain-shift refinement (an affine fit restricted to a subcortical region). Against ANTs on
  phantoms: an affine fit from the centres of mass within 0.5 mm of ANTs', a masked one no
  farther from the truth than ANTs' (`tests/test_linear.py`). On the Lead-Tutor cohort, as
  leaddbs-py's brain-shift correction, the masked fit lands 0.19 mm from ANTs' (as close as two
  ANTs runs land to each other) in 2.4-4.8 s instead of 5-11 s. `check_linear_stage` replaces
  `check_rigid_stage`.

### dbsloc-gpureg 0.3.0 (2026-11-08)

- `rigid_session` / `RigidSession` and `register_rigid`: rigid registration on the GPU (one
  `Rigid` stage on Mattes MI from any start, an optional fixed-image mask, winsorizing), for
  coregistration. A session prepares and uploads the image pair once and keeps its smoothed
  copies between fits, so a multi-start of five coarse and two full fits costs little more than
  one: a post-op CT onto a T1w anchor in 17 s on a laptop GPU where ANTs takes 32 s, the fits
  within 0.7 mm of ANTs' over a head-sized cube. `check_rigid_stage` checks a stage without a GPU.

### dbsloc-gpureg 0.2.1 (2026-11-07)

- Fix: on Linux with an NVIDIA GPU, the GPU disappeared after ten registrations (or `gpu_info`
  calls) in one process: every call created and destroyed a Vulkan instance, the loader unloaded
  NVIDIA's driver library each time, and from the eleventh load on it no longer provided
  `vkCreateInstance`. A cohort run in one process fell back to the CPU after ten subjects. One idle
  Vulkan instance now lives for the whole process, so the driver stays loaded (no device, no GPU
  memory). Measured on an RTX A4000, driver 580: 30 `gpu_info` calls and 25 registrations in one
  process, all on the GPU.

### dbsloc-gpureg 0.2.0 (2026-11-07)

- `register_syn(recipe=...)`: the caller sets the whole schedule (Rigid, Affine, SyN: iterations,
  shrink factors, smoothing, metric parameters, channel weights) instead of the built-in cohort
  recipe; `cohort_recipe(channels)` returns that recipe in the same form, and the result reports
  the recipe that ran. Left out, it is the cohort recipe as before, bit for bit. A recipe is
  checked before any work, including the GPU limits (32 MI bins, CC radius at most 8).
- `compose_field` and `apply_transforms`: what a caller does with the composites after the
  registration (`antsApplyTransforms --compose`, and resampling with the linear, nearest or
  Lanczos windowed-sinc interpolator), in parallel Rust: about 10x faster than ANTs, equal to
  `float32` rounding. `compression_level=0` writes the field as stored gzip, which ANTs reads back
  about 7x faster than its own compressed `float64` field.

### dbsloc-gpureg 0.1.1 (2026-11-07)

- float32 images are passed to the engine without copies (a view) and converted to float64
  once: about 130 MB instead of 660 MB extra memory per 0.5 mm template.
- An internal engine panic is raised as `RegistrationError` instead of `PanicException`, so
  callers that catch `Exception` can fall back.
- A missing output folder is reported before the registration runs; the Python wrapper creates
  it.
- The package version is read from `Cargo.toml` only; CI runs a RustSec dependency audit.
- New tests: determinism, float32 vs float64, NaN input, output folder, integer input.

### dbsloc-gpureg 0.1.0 (2026-11-06)

- First version: the dbs-localizer GPU registration engine (commit `00faf42`) as a Python
  wheel. `register_syn` runs the cohort recipe (centre of mass, Rigid, Affine, SyN; one or two
  channels) and writes ANTs `Composite.h5` / `InverseComposite.h5` / `0GenericAffine.mat`.
- GPU capacity check before any work (`GpuCapacityError`), `fits_gpu`, `gpu_info`,
  `gpu_available`; cancellation from another thread (`CancelToken`); the GIL is released while
  the engine runs.
- Wheels for macOS (arm64, x86_64), Linux (x86_64, aarch64, manylinux 2.28) and Windows
  (x86_64); one abi3 wheel per platform for Python ≥ 3.10.
