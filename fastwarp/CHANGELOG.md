# Changelog

## Unreleased

## 0.2.0 (2026-11-15)

- ANTs compatibility mode: `fastwarp ants-registration ARGS...` and
  `fastwarp ants-apply-transforms ARGS...` take the arguments of `antsRegistration` and
  `antsApplyTransforms` (ANTs 2.6.5), run the subset FastWarp supports and write the files ANTs
  writes under ANTs' names: `0GenericAffine.mat`, `1Warp.nii.gz`, `1InverseWarp.nii.gz` (or
  `0Warp.nii.gz` for SyN without `-r`), `Composite.h5` / `InverseComposite.h5` alone with `-a 1`,
  and the warped and inverse-warped images of `-o [prefix,warped,inverseWarped]` on the native
  grids. Supported: any Rigid / Affine stages with Mattes MI (regular sampling, 32 bins), then
  at most one SyN stage with one or two CC metrics; `-r [f,m,1]`, `Identity` or an affine file;
  fixed masks on linear stages; `-w`; `--random-seed` and `ANTS_RANDOM_SEED`; both stage-option
  orders and brackets split over shell words. Warped images and fields are `float64`, or
  `float32` with `--float 1`, as in ANTs. Everything that would change the result and is not
  supported (MI in SyN, dense or random MI sampling, `-z 0`, histogram matching, SyN masks,
  moving masks, other transforms, metrics and interpolators, ...) is refused with exit status 2
  and the reason, before the GPU is opened; `ants-apply-transforms` accepts `-z`, `--time-index`
  and `-u` with a transform output, which change nothing, with a warning.
  The compatibility mode is reached only through these subcommands, never by the program's name.
  The registration grid is the fixed image's smallest spacing; with an anisotropic fixed image the
  warp fields are resampled onto the fixed image's grid, where ANTs writes them, so the result
  differs a little more from ANTs' there (about 1 mm at the corners of the field of view in the
  tests, against 0.06 to 0.5 mm for an isotropic one). Stage options are assigned to stages as
  ANTs assigns them. Integer `-u` outputs of `ants-apply-transforms` wrap as in ANTs. The parsers
  are pure functions (`fastwarp_cli::ants::parse_registration` / `parse_apply`, and
  `resolve_seed` for `--random-seed` / `ANTS_RANDOM_SEED`) that return the plan, its recipe and
  its output files; the Python layer `fastwarp.ants` runs them through the same code. See
  `docs/cli.md`.

- Linear stages skip a level whose sample points leave Mattes mutual information undefined
  (no point on the fixed image, or one fixed value only; typical for a coarse level of a small
  image or a small mask) and continue with the finer levels, with a warning: on standard error
  in the command line (`register`, `linear`, `ants-registration`), and as a `UserWarning` in
  Python, also listed under `"warnings"` in the results of `register_syn` and
  `LinearSession.register` (the result of `fastwarp.ants.registration` keeps ANTsPy's keys).
  ANTs 2.6.5 drops the whole stage in that case without an error.
  When every level of a stage is skipped, the run fails and the message names the levels, their
  shrink factors, the candidate counts and the remedies.

- `fastwarp.ants`: ANTsPy's `ants.registration` and `ants.apply_transforms` (ANTsPy 0.6.3) with
  their arguments and results, for the part FastWarp supports. Each call builds the argument list
  ANTsPy builds (compared token for token in the tests) and runs it with the ANTs compatibility
  mode, the same code as `fastwarp ants-registration` / `ants-apply-transforms`, with the
  `ANTsImage`s handed over in memory. Runs `Rigid`, `Affine`, `AffineFast`, `BOLDAffine`,
  `QuickRigid`, `DenseRigid`, `BOLDRigid`, `SyNCC`, `SyNabp`, `antsRegistrationSyN[r|a|s|sr|so]`,
  `antsRegistrationSyNQuick[r|a]`, and `SyN`, `SyNRA`, `SyNOnly`, `SyNAggro`, `SyNLessAggro`,
  `ElasticSyN`, `SyNBold`, `SyNBoldAff` with `syn_metric="CC"`. ANTsPy's default `"SyN"` (Mattes
  mutual information in SyN), Translation, Similarity, BSplineSyN, GC, time-varying and other
  unsupported arguments (SyN masks, moving masks, histogram matching, other interpolators, ...)
  raise `fastwarp.ants.UnsupportedArgument` (a `ValueError`) before any work; arguments ANTsPy
  ignores for a type are ignored with a `UserWarning`; unknown keyword arguments raise
  `TypeError`. The result dict has ANTsPy's keys, warped images on the native grids with the
  fixed image's pixel type, and the transform lists in ANTsPy's order. Keyword-only additions:
  `iso_mm` (default: the fixed image's smallest spacing), `adapter`, `cancel` and
  `fallback="ants"`, which runs ANTsPy instead after a refusal, without a GPU or when the
  registration does not fit, with a warning and an `"engine"` key in the result. A raw argument
  list, `registration([...], None)`, runs as the subcommand does. `outprefix=""` makes a private
  temporary folder instead of ANTsPy's `tempfile.mktemp` name. ANTsPy is needed at call time
  only. See `docs/guide/ants.md`.

- Stage lists: `register_syn(recipe={"initial": ..., "stages": [...]})` runs any number of
  Rigid / Affine stages followed by at most one SyN stage (SyN alone is allowed), from the centres
  of mass (`"com"`, the default), the identity (`"identity"`), one affine from a `.mat` / `.txt`
  file (optionally inverted) or given parameters. Without a SyN stage the composites hold only the
  affine, as ANTs writes them. The capacity check (in `register_syn`, `fits_gpu(recipe=...)` and
  `fastwarp fits --recipe`) covers every stage: a linear-only chain is checked against the memory
  its sample buffers need, not the SyN stage's, and a chain with both is refused when the larger
  of the two phases does not fit. A relative path of the start file is read from the current
  folder in Python and from the recipe file's folder in `fastwarp register --recipe`. `None` for
  `initial` or `stages` means the default. The existing recipe form keeps working and gives
  bit-identical outputs. The result dict adds `initial_parameters`, `initial_center`,
  `linear_stages`, `deformable` and `files`.
- The recipe key `"winsorize"`: `[lower, upper]` quantiles (default `[0.005, 0.995]`, as before) or
  `None` for ANTs' plain rescale to [0, 1], what `antsRegistration` (and every ANTsPy call) does
  without `--winsorize-image-intensities`. `LinearSession(winsorize=None)` is unchanged: it uses
  the intensities without any rescale; `winsorize=(0.0, 1.0)` is ANTs' default preprocessing.
- `register_syn(outputs="split" | "both")` (`fastwarp register --outputs`) writes ANTs'
  non-composite files `0GenericAffine.mat`, `1Warp.nii.gz`, `1InverseWarp.nii.gz`; the fields are
  `float32` (ANTs writes `float64`).
- `apply_transforms(..., invert=[...])` and `compose_field(..., invert=[...])`, ANTsPy's
  `whichtoinvert` (one flag per transform), and `fastwarp apply` / `compose -t [file,1]`, as
  `antsApplyTransforms -t [file,1]`: a linear transform is replaced by its inverse in place.
  Inverting a displacement field raises `ValueError` (exit status 2) instead of ANTs' silent exit 1.
  `fastwarp apply` / `compose` and `compose_field` read and check the transforms before they load
  an image or create the output folder.
- A command line, `fastwarp` (also `python -m fastwarp`), for NIfTI files: `register`
  (`register_syn`), `linear` (one rigid or affine fit), `apply` (`apply_transforms`), `compose`
  (`compose_field`), `recipe` (the default recipe as JSON), `gpu-info`, `fits` and `bench` (timings
  on built-in synthetic volumes or on your images, with a description of the machine). Images are
  read as ITK and ANTsPy read them. Distinct exit status for invalid arguments (2), file errors (3),
  no usable GPU (4) and a grid too large for the GPU (5); `--json` for machine-readable output.
  An internal error prints one line and exits with status 1; Rust's panic message is shown only
  when `RUST_BACKTRACE` is set to a value other than 0. `register` and `linear` read their input images before they create
  the output folder. See `docs/cli.md`. The Python extension and the standalone program built from the new crate
  `fastwarp-cli` run the same code.
- Each GitHub release carries the standalone `fastwarp` program, which needs no Python, for Linux
  x86_64 and aarch64 (glibc 2.28 or later), macOS arm64 and x86_64 (macOS 11 or later) and Windows
  x86_64 (static C runtime), as `fastwarp-<target>.tar.gz` / `.zip` with a `.sha256` file each,
  and the installer scripts `fastwarp-installer.sh` (Linux, macOS) and `fastwarp-installer.ps1`
  (Windows). The installer of a release installs that release and holds the SHA-256 of its
  archives, so it refuses a download that does not match. It installs without administrator
  rights into `~/.local/bin` (Windows: `%LOCALAPPDATA%\Programs\fastwarp\bin`), adds that folder
  to PATH unless told not to (`--no-modify-path`, `-NoModifyPath`), and records what it did in a
  receipt next to the program; `--uninstall` (`-Uninstall`) undoes exactly that and leaves alone a
  `fastwarp` it did not install. Every release file has a GitHub build provenance attestation
  (`gh attestation verify`). See `docs/cli.md`.
- `fits_gpu` takes the moving image(s) as an optional second argument and checks the moving grid
  as `register_syn` does; without it a moving image larger than the fixed one could pass
  `fits_gpu` and still raise `GpuCapacityError`. `channels` defaults to the number of fixed
  images, and invalid arguments (image counts, `channels` other than 1 or 2, `iso_mm`) raise
  `ValueError` as in `register_syn` instead of returning a result.
- The benchmark (`benchmarks/accuracy`) now also measures speed in the same run, on the same
  public data: every registration records its Linear, SyN and total time for FastWarp and for
  ANTs (read from `antsRegistration --verbose 1`); `linear` times `linear_session` fits
  (rigid, affine, a rigid multi-start) against `antsRegistration` with the same stages, with
  their landmark errors; `post` times `compose_field` and `apply_transforms` against
  `antsApplyTransforms` on uncompressed outputs (`--gzip` adds `.nii.gz`). `report` tabulates
  the median and quartiles of each, per dataset and registration grid, with the machine and
  software they ran on (CPU, GPU and its driver name, OS, fastwarp and ANTs versions, the thread
  setting and core counts of each command). Both engines use all cores unless `--threads`
  caps both alike, and the benchmark no longer leaves `ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS` set
  in the calling process.
- `docs/performance.md` explains how the speed is measured and how to reproduce it; the
  earlier figures, measured on non-public data, are labelled as such with what is known about
  them.
- An accuracy benchmark that reviewers can run (`benchmarks/accuracy`, `docs/benchmarks.md`):
  FastWarp and ANTs register the same pairs with the same recipe on public data with manual
  cortical, subcortical and deep-nuclei labels and anatomical fiducials; every download is checked
  against a pinned SHA-256. A `benchmark` dependency group adds SciPy and Matplotlib (already
  locked as ANTsPy dependencies). No results yet.
- README badges: CI, PyPI version, Python versions, licence, documentation.
- The documentation is reworked: the PyData Sphinx theme with light and dark modes, a user guide
  in short pages (how it works, images, normalization, output files and the direction each one
  maps, recipes with the type, unit and limits of every field, linear fits, resampling, GPU
  memory, limitations), troubleshooting, a glossary, reference pages for the API, the
  environment variables and the errors, and project pages for citing, security, the code of
  conduct and related projects. The README is shorter and links into it. The API reference moved
  from `api/index.html` to `api.html`.
- The docstrings of the public API are rewritten in NumPy style, with parameters, defaults,
  units, return values and the exceptions each function raises.
- `dir(fastwarp)` lists only the public API (`__all__`); the implementation moved to the private
  module `fastwarp._api`, and the public Python functions report `fastwarp` as their module.
- `LinearSession.register` creates the folder of `out_prefix`, as `register_syn` and
  `register_rigid` do.
- An `out_prefix` that ends in a path separator (`/`, and on Windows also `\`) names a folder, as
  in ANTs: `register_syn(..., out_prefix="out/sub/")` writes `out/sub/Composite.h5` (was
  `out/subComposite.h5`), and the folder `out/sub` is created. The same holds for
  `LinearSession.register` and the deprecated `register_rigid` (#28). A prefix of `.` or `..` is
  text too, as in ANTs (`.` gives `.Composite.h5`): use `./` for the current folder. Breaking for
  Rust callers of the engine crate `fastwarp-reg`: `transform::ants_io::with_suffix` (and so
  `write_outputs`, `write_linear_output` and `write_composite_pair` / `read_composite_pair`)
  appends the suffix to the prefix as text instead of to its file name, and `fastwarp_reg_syn
  --out` follows the same rule.
- `GpuCapacityError`, `GpuUnavailableError`, `RegistrationCancelled` and `RegistrationError`
  report `fastwarp` as their module (was `_core`, which cannot be imported), so they pickle and
  cross process boundaries, for example from a `concurrent.futures.ProcessPoolExecutor` worker.
  `gpu_info` reports `fastwarp` as well (#26).
- The GPU capacity check (`fits_gpu`, `fastwarp fits` and the check before `register_syn`) no
  longer underestimates small grids: it now counts the buffers the coarser SyN levels and the
  device self-test leave allocated, which the finest level's bytes per voxel missed (on a
  48 x 48 x 48 grid the check planned 18.6 MB for a run that peaked at 20.9 MB). Small grids near
  the budget can now be refused where they passed before; on a 1 mm head the figure grows by
  under 2%. Its error now reads "the SyN stage on the registration grid ... needs ... bytes".

### Deprecated

- `register_rigid`, `rigid_session` and `RigidSession` still work but emit a `DeprecationWarning`
  and will be removed in fastwarp 0.3.0; use `linear_session(...).register()`,
  `linear_session` and `LinearSession`. They are no longer in `fastwarp.__all__` or `dir(fastwarp)`.

## 0.1.0 (2026-11-12)

First public release.

- `register_syn`: ANTs-style registration on the GPU (centre of mass, Rigid, Affine, SyN) of one
  image pair, or of two channels such as T1w + T2w, onto a fixed image. It writes ANTs
  `Composite.h5`, `InverseComposite.h5` and `0GenericAffine.mat`, which `antsApplyTransforms`
  reads. The schedule is the built-in recipe (`cohort_recipe(channels)`) or one the caller passes
  as `recipe=`; the result reports the recipe that ran and the time of each phase. Images are
  ANTsPy images or `(array, spacing, origin, direction)` tuples in ITK (LPS) geometry, so ANTsPy
  is optional; the registration runs on an isotropic grid of `iso_mm` (default 1 mm), whose size
  `iso_grid_dims` reports.
- `linear_session` / `LinearSession`: rigid or affine fits on Mattes mutual information from any
  start, with an optional fixed-image mask and winsorizing. A session uploads the image pair once
  and keeps its smoothed copies between fits, so a multi-start search costs little more than one
  fit. `register_rigid` is a one-off rigid fit; `rigid_session` / `RigidSession` are aliases of
  `linear_session` / `LinearSession`.
- `compose_field` and `apply_transforms`: write a list of ANTs transform files (`.h5`, `.mat`,
  `.txt`, displacement fields) as one displacement field (`antsApplyTransforms --compose`;
  `compression_level=0` writes stored gzip, which ANTs reads back quickly), and resample images
  with the linear, nearest or Lanczos windowed-sinc interpolator, in parallel on the CPU.
- GPU checks before any work: `gpu_available`, `gpu_info`, `fits_gpu`, and a capacity check that
  raises `GpuCapacityError` when a registration would not fit the GPU's memory budget
  (`FASTWARP_GPU_MEMORY_BUDGET_MB` overrides it). Failures raise `GpuUnavailableError`,
  `GpuCapacityError` or `RegistrationError` (all `RuntimeError`), an internal panic included, so
  a caller can fall back to ANTs; a cancelled run raises `RegistrationCancelled`; invalid
  arguments raise `ValueError` before any work.
- `CancelToken` cancels a running registration from another thread; the GIL is released while
  the engine runs.
- Runs on Vulkan, Metal and DirectX 12 through wgpu; `FASTWARP_GPU_BACKEND` and
  `FASTWARP_GPU_ADAPTER` choose the API and the device. A hardware GPU is required: software
  adapters (lavapipe, WARP) are refused unless `FASTWARP_GPU_ALLOW_SOFTWARE=1`. The README lists
  every environment variable.
- The HDF5 transform reader rejects malformed or crafted files with an error instead of
  exhausting memory or aborting the process.
- Wheels for Linux (x86_64, aarch64; manylinux 2.28), macOS (arm64, x86_64) and Windows
  (x86_64): one `abi3` wheel per platform for Python 3.11 and later, with NumPy 2.0 or later.
- Apache-2.0; parts are ports of ITK and ANTs, and the sample-point generator derives from
  BSD-3-Clause Mersenne Twister code; all are listed in `NOTICE` and `docs/THIRD_PARTY_PORTS.md`.
- The engine was developed inside Wyss DBS Localizer under the name `dbsloc-gpureg`, which was
  never published. Code written against it imports `fastwarp` instead, with the same Python API,
  on Python 3.11 or later and NumPy 2.0 or later. The former `DBSLOC_*` environment variables are
  still read when the `FASTWARP_*` name is unset (a `FASTWARP_*` name set to an empty value
  wins). `ENGINE_SOURCE` (`"fastwarp-legacy 90c1df3"`) names the revision of that earlier
  standalone copy of the engine, not a revision of this repository.
