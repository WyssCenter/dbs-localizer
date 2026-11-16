# Performance

This page says how FastWarp's speed is measured against ANTs, how to repeat the measurement,
and where the results come from. The measurements are part of the
[accuracy benchmark](benchmarks.md): the same run that scores accuracy on public data also
times every registration, a set of linear fits and the post-processing, and records the machine
and software it ran on.

No benchmark results are reported here yet. The tables below will be the `report` output of a
full run, with its machine block. Until then, the only numbers on this page are FastWarp's
earlier measurements on non-public data ([below](#earlier-measurements-on-non-public-data)),
which a reader cannot repeat.

To time FastWarp alone on your own machine, without ANTs and without downloading data, run
`fastwarp bench` (see [Command line](cli.md#bench)): it registers built-in synthetic volumes and
prints the machine it ran on.

## What is measured

All times are wall-clock seconds. ANTs runs in a child process, one at a time, never at the same
time as FastWarp. Both use all cores unless `--threads N` caps both alike; timings with
different settings are kept apart. Each record also says which machine and software it ran on.
[Timings](benchmarks.md#timings) on the benchmark page defines every field and what it
includes.

| Measurement | Benchmark command | FastWarp | ANTs | Data |
| --- | --- | --- | --- | --- |
| Registration | `run` | {func}`~fastwarp.register_syn` | `antsRegistration` with the same recipe | every pair of the benchmark (default 50) |
| Linear fits | `linear` | {func}`~fastwarp.linear_session`: rigid, affine, a rigid multi-start | `antsRegistration` with the same rigid and affine stages | `snsx` (default 20 pairs) |
| Post-processing | `post` | {func}`~fastwarp.compose_field`, {func}`~fastwarp.apply_transforms` | `antsApplyTransforms` | the pairs `run` registered |

Each step also records an accuracy figure: the registrations are scored against manual labels
and landmarks (`evaluate`), the linear fits by their landmark error and their distance from
ANTs' fit, and the post-processing by the difference between the two tools' outputs.

The recipe is `--recipe cohort`, FastWarp's default ({func}`fastwarp.cohort_recipe` with one
channel): one {term}`T1-weighted <T1w, T2w>` image per side, Rigid and Affine on
{term}`Mattes mutual information`, {term}`SyN` on {term}`cross-correlation`, on a 1 mm grid. None of the public datasets
has a T2w aligned to the T1w, so the benchmark does not time the two-channel registration.

## How to reproduce

Install as in [Running it](benchmarks.md#running-it) (Python 3.13, the `benchmark` dependency
group, FastWarp from the checkout or from PyPI), then, with folders outside the repository:

```bash
B="uv run --no-sync python -m benchmarks.accuracy"
$B fetch --data ~/fastwarp-bench/data
$B run --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work
$B linear --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work
$B post --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work
$B evaluate --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work
$B report --work ~/fastwarp-bench/work
```

`--ants-dir DIR` runs the ANTs programs in `DIR` (for example a release of ANTs 2.6.5) instead of
the ones bundled with ANTsPy; `--adapter NAME` picks a GPU. Use a machine that does nothing
else during the run; `--threads N` caps both engines' CPU threads if the machine is needed
meanwhile, and its timings are then reported separately. `plan` estimates the time left; every
step resumes where it stopped.

## Where the results are

- `<work>/results/cohort/summary.md`: the tables to paste here, with the machine and software
  (CPU, GPU and its driver name, operating system, fastwarp and ANTs versions, thread setting of
  each command, the benchmark's git commit);
- `timings.csv`, `linear.csv`, `post.csv` beside it: one row per registration, fit or step;
- the records they come from: `<work>/<dataset>/cohort/<pair>/{fastwarp,ants}/run.json`,
  `<work>/<dataset>/linear/<pair>/` and `<work>/<dataset>/cohort/<pair>/post.json`, each with
  the `environment` it ran in.

## Results

To be added from the first full run: the `Registration time`, `Linear fits` and
`Post-processing` tables of `summary.md`, with its machine block.

## Earlier measurements on non-public data

FastWarp's own timings from before the benchmark were measured on data of a non-public cohort,
with scripts that are not in this repository, so they cannot be repeated. They will be replaced
by the benchmark results. ANTs timings from the same period are not shown, because how they were
made (ANTs version, command lines, machine) is not documented.

:::{dropdown} Earlier measurements on non-public data
Registration: a T1-weighted and a T2-weighted scan of one subject (the T1w as acquired, the T2w
aligned to it by one rigid fit) onto the MNI 2009b template (0.5 mm), on a 1 mm registration
grid, with the default recipe and a development build made before the first release (0.1.0).
Each figure is the median of three runs of the engine's own `linear_s`, `syn_s` and `total_s`
(2026-11-09).

| Hardware | Linear | SyN | Registration total |
| --- | --- | --- | --- |
| NVIDIA RTX A4000 (Linux, Vulkan) | 3.5 s | 6.4 s | 12.4 s |
| Apple M5 Pro (MacBook Pro, Metal) | 4.1 s | 9.4 s | 13.7 s |
| Intel Lunar Lake integrated GPU (Linux laptop, Vulkan) | 3.5 s | 16.4 s | 21.0 s |
| AMD Radeon Pro 5500 XT (Intel iMac, Metal) | 6.1 s | 16.7 s | 23.5 s |
| NVIDIA Quadro K620, 2 GB (Windows Server 2022, Vulkan), T1w only (see below) | 10.6 s | 54.7 s | 67.3 s |

The total includes preparing and uploading the images. The A4000 machine was under heavy CPU
load (load average about 110 on 48 cores) during its runs. On the Quadro K620, 2 GB is too
little for T1w + T2w at 1 mm, which needs 1.54 GiB of a budget that keeps 1 GiB free for the
desktop: the capacity check refuses it before any work ({class}`~fastwarp.GpuCapacityError`).
T1w alone needs 1.38 GiB and ran only with `FASTWARP_GPU_MEMORY_BUDGET_MB=1900`.

Linear fits: one rigid fit of the T2w onto the T1w (the default rigid stage) took 1.2 s on the
M5 Pro, 1.6 s on the RTX A4000, 1.8 s on the Radeon Pro 5500 XT, 2.1 s on Lunar Lake and 2.7 s
on the Quadro K620. Aligning a CT scan taken after surgery onto the subject's T1w scan (a
{term}`coregistration`), with five short rigid fits from different starting positions and two
full ones, took 17 s on the laptop GPU. An affine fit limited by a mask to part of the brain
took 2.4 to 4.8 s on the RTX A4000. The subjects behind these ranges are not documented.

Post-processing, on a laptop with 8 threads: turning the composites into displacement fields on
the 0.5 mm template and subject grids, and a Lanczos re-warp of the subject image. Which laptop
and which script produced these figures is not documented.

| Step | {func}`~fastwarp.compose_field` / {func}`~fastwarp.apply_transforms` |
| --- | --- |
| Forward field (69 million voxels) | 1.4 s |
| Inverse field (37 million voxels) | 0.95 s |
| Lanczos re-warp | 2.7 s |
:::
