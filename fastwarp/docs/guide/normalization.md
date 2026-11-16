# Normalization

{func}`~fastwarp.register_syn` aligns a {term}`moving image` to a {term}`fixed image` with a
{term}`rigid`, an {term}`affine` and a deformable {term}`SyN` stage, in one call, and writes the
result as ANTs transform files. A [stage-list recipe](recipes.md#stage-lists) can choose
another start and other stages, for example only the linear ones. All parameters:
{func}`fastwarp.register_syn` in the API reference.

| Argument | Default | Meaning |
| --- | --- | --- |
| `fixed`, `moving` | required | One [image](images.md) each, or a list of two images each (see [Two channels](#two-channels)). |
| `out_prefix` | required | Where the files go: a file-name prefix, or a folder when it ends in `/` (see [Output prefix](#output-prefix)). |
| `iso_mm` | `1.0` | Voxel size of the {term}`registration grid`, in millimetres. |
| `write_warped` | `False` | Also write the warped moving image. |
| `adapter` | `None` | Use the GPU whose name contains this text (case-insensitive). `None` uses `FASTWARP_GPU_ADAPTER` if it is set, and otherwise lets FastWarp choose. |
| `cancel` | `None` | A {class}`~fastwarp.CancelToken` to stop the run from another thread. |
| `recipe` | `None` | The registration schedule; `None` uses the default. See [Recipes](recipes.md). |
| `outputs` | `"composite"` | Which transform files to write: `"composite"`, `"split"` or `"both"`, one of {data}`~fastwarp.OUTPUT_LAYOUTS`. See [Output files](outputs.md). |

## What happens in a run

The steps of a run, in order. Steps 4 and 5, with a blue frame, run on the GPU; the others run on
the CPU.

```{raw} html
<div class="fw-flow" role="list" aria-label="Steps of a register_syn run">
  <div class="fw-step" role="listitem"><strong>1. Check</strong>arguments, recipe, GPU and its memory</div>
  <div class="fw-step" role="listitem"><strong>2. Prepare</strong>resample to the registration grid, winsorize</div>
  <div class="fw-step" role="listitem"><strong>3. Centre of mass</strong>starting translation</div>
  <div class="fw-step fw-gpu" role="listitem"><strong>4. Rigid, Affine</strong>mutual information, first image</div>
  <div class="fw-step fw-gpu" role="listitem"><strong>5. SyN</strong>cross-correlation, every image</div>
  <div class="fw-step" role="listitem"><strong>6. Write</strong>Composite.h5 and the other files</div>
</div>
```

1. **Check.** The arguments and the recipe are checked; a mistake raises `ValueError`. Then the
   GPU is opened and the [capacity check](gpu.md#what-fits) runs. If there is no usable GPU, or
   the grid does not fit, the call raises {class}`~fastwarp.GpuUnavailableError` or
   {class}`~fastwarp.GpuCapacityError`. No transform has been computed or written at this
   point; only the output folder may have been created.
2. **Prepare.** Every image is resampled to the spacing of the registration grid (see below).
   Then its intensities are {term}`winsorized <winsorize>`: by default the darkest 0.5 % and the
   brightest 0.5 % of the voxels are clipped to the 0.005 and 0.995 quantiles, and the result is
   rescaled to [0, 1]. The recipe key `"winsorize"` changes the quantiles, or with `None` only
   rescales (see [Winsorizing](recipes.md#winsorizing)). There is no histogram matching.
3. **Start.** By default the {term}`centres of mass <centre of mass>` of the first fixed and the
   first moving image give the starting translation, as
   `antsRegistration --initial-moving-transform [fixed,moving,1]` does. They are computed on the
   resampled images before winsorizing. A stage-list recipe can start from the identity or from
   a given affine instead.
4. **Rigid, then Affine,** on the GPU: {term}`Mattes mutual information` on the first image of
   each side.
5. **SyN,** on the GPU: {term}`cross-correlation` on every image, starting from the affine.
6. **Write.** The first moving image is warped onto the registration grid (the `warp_s` step),
   and the files are written.

The [recipe](recipes.md) sets the iterations, resolution levels and metric settings of steps 4
and 5, the start of step 3 and the winsorizing of step 2. A stage-list recipe can also leave
out steps 4 or 5, or repeat step 4 with other settings; the capacity check of step 1 then
covers exactly those stages.

## The registration grid

The registration runs on an isotropic grid with `iso_mm` spacing, built from the first fixed
image: it has the fixed image's origin and direction, and along each axis
`floor((n - 1) * spacing / iso_mm) + 1` voxels, where `n` and `spacing` are the fixed image's
size and spacing along that axis. An axis whose spacing already equals `iso_mm` keeps its size.
{func}`~fastwarp.iso_grid_dims` returns the size:

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
print(fastwarp.iso_grid_dims(fixed, 1.0), fastwarp.iso_grid_dims(fixed, 2.0))
```

```{code-block} text
:class: fw-output
(182, 218, 182) (91, 109, 91)
```

The moving images are resampled to the same spacing on their own grids. The displacement fields
in the output files lie on the registration grid. A coarser grid (a larger `iso_mm`) needs less
GPU memory and time; a finer one resolves finer detail. [GPU and memory](gpu.md#what-fits) gives
the memory a grid needs.

## Output prefix

`out_prefix` is joined to the file names as text, as in ANTs: the prefix `out/sub_` gives
`out/sub_Composite.h5`, `out/sub_InverseComposite.h5`, `out/sub_0GenericAffine.mat` and, with
`write_warped=True`, `out/sub__Warped.nii.gz` (two underscores, because the prefix ends in one).
With `outputs="split"` or `"both"` the names are those of ANTs' separate files, such as
`out/sub_1Warp.nii.gz`. [Output files](outputs.md) explains what each file holds and which
direction it maps; the result's `files` lists the paths written.

- A prefix that ends in a path separator names a folder: `out/sub/` gives `out/sub/Composite.h5`
  and so on. The separator is `/`, and on Windows also `\`.
- Pass a folder prefix as a string. `pathlib.Path("out/sub/")` drops the trailing separator and
  becomes the prefix `out/sub`, which gives `out/subComposite.h5`.
- `.` and `..` are text as well: the prefix `.` gives `.Composite.h5`. Use `./` for the current
  folder.
- The folder the files go to is created when it does not exist.

## Result

{func}`~fastwarp.register_syn` returns a dictionary:

| Key | Type | Content |
| --- | --- | --- |
| `adapter` | `str` | The GPU, its graphics API and type, for example `"Intel(R) Arc(TM) 140V GPU (16GB) (Vulkan, IntegratedGpu)"`. |
| `grid` | `list[int]` | Voxels per axis of the registration grid. |
| `total_s` | `float` | Seconds from opening the GPU to the end of writing the files, including preparing and uploading the images. |
| `resample_s` | `float` | Seconds to resample the images to the registration grid. |
| `preprocess_s` | `float` | Seconds to winsorize and rescale the images. |
| `linear_s` | `float` | Seconds for the centre-of-mass start and the Rigid and Affine stages. |
| `syn_s` | `float` | Seconds for the SyN stage. |
| `warp_s` | `float` | Seconds to warp the first moving image onto the registration grid. |
| `write_s` | `float` | Seconds to write the output files. |
| `levels` | `list[tuple]` | One tuple per SyN level, coarsest first (see below); empty without a SyN stage. |
| `affine_parameters` | `list[float]` | The 12 parameters of the affine (the 3 × 3 matrix row by row, then the translation in millimetres): the start and every linear stage combined, as in `0GenericAffine.mat`. |
| `affine_center` | `list[float]` | The centre of rotation of that affine, in LPS millimetres. |
| `initial_parameters`, `initial_center` | `list[float]` | The start of the chain: the centre-of-mass translation, the identity, or the given affine. |
| `linear_stages` | `list[dict]` | One dict per linear stage, in order: `transform` (`"rigid"` or `"affine"`), `iterations` per level, and `metric`, the final negative mutual information (`None` when the last level ran no iteration). |
| `deformable` | `bool` | Whether a SyN stage ran. |
| `files` | `list[str]` | The paths written, in the order composites, `0GenericAffine.mat`, warps, warped image. |
| `peak_gpu_bytes` | `int` | The most GPU memory the engine's buffers held at one time during the run. |
| `memory_budget_bytes` | `int` | The memory budget the capacity check used. |
| `recipe` | `dict` | The full recipe that ran, every field present. |
| `warnings` | `list[str]` | One entry per linear level skipped because its sample points leave mutual information undefined; each is also emitted as a `UserWarning`. Empty when every level ran. |

Each entry of `levels` is a tuple `(level, grid_size, iterations, converged, energy)`:

| Item | Type | Meaning |
| --- | --- | --- |
| `level` | `int` | Index of the level, from 0 (the coarsest). |
| `grid_size` | `list[int]` | Voxels per axis of the level's grid. |
| `iterations` | `int` | Iterations the level ran. |
| `converged` | `bool` | `True` when the convergence test stopped the level before its iteration limit. |
| `energy` | `float` | The metric value of the last iteration, the negative cross-correlation of the first channel (lower is better); 0.0 when the level ran no iteration. |

## Two channels

Pass two images on each side to use two {term}`channels <channel>` in the SyN stage, for example
a {term}`T1-weighted and a T2-weighted <T1w, T2w>` scan:

```python
info = fastwarp.register_syn(
    [template_t1, template_t2], [subject_t1, subject_t2], "out/sub_"
)
```

The names are placeholders for your own images. The Rigid and Affine stages use only the first
channel; SyN uses both, each with the weight in `channel_weights` (0.5 each by default). The
second moving image must already be aligned to the first one, for example by a rigid fit with
{func}`~fastwarp.linear_session`. Two channels need more GPU memory; pass both lists to
{func}`~fastwarp.fits_gpu` to check.

## Cancelling a run

A {class}`~fastwarp.CancelToken` stops a run from another thread. The run stops at its next
iteration and raises {class}`~fastwarp.RegistrationCancelled`; no files are written. Cancelling
the token before the run starts also raises it.

```python
import threading

import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))
token = fastwarp.CancelToken()
outcome = {}


def run():
    try:
        outcome["info"] = fastwarp.register_syn(fixed, moving, "out/cancel_", cancel=token)
    except fastwarp.RegistrationCancelled:
        outcome["cancelled"] = True


worker = threading.Thread(target=run)
worker.start()
threading.Timer(2.0, token.cancel).start()
worker.join()
print(outcome)
```

```{code-block} text
:class: fw-output
{'cancelled': True}
```

A token stays cancelled; use a new one for the next run. `token.cancelled` tells whether
`cancel()` has been called.

## Threads and repeat runs

While the engine runs, the call lets other Python threads run (it releases Python's global
interpreter lock). Two runs with the same inputs on the same GPU give the same transforms
(`tests/test_register.py::test_runs_are_deterministic`).

## The figure on the home page

The figure on the [home page](../index.md) shows a 7 T T1-weighted scan of subject C001 of the
public [SNSX dataset](https://openneuro.org/datasets/ds004470/versions/1.0.1) (CC0) registered
with the default settings onto the MNI ICBM 2009c template from
[TemplateFlow](https://www.templateflow.org). The middle and right panels alternate squares of
the template and of the subject's scan, before and after the registration; where the
registration is good, edges continue across the squares. For the display only, the subject's
intensities were matched to the template's. The script `docs/_scripts/make_figures.py` in the
repository downloads both files, checks them against the SHA-256 that the
[accuracy benchmark](../benchmarks.md) pins, and makes the figure:

```bash
uv run --no-sync python docs/_scripts/make_figures.py --cache ~/fastwarp-figures
```
