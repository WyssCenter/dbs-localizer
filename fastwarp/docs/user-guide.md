# User guide

The examples assume `import fastwarp` and ANTsPy images named `fixed`, `moving`, `reference`
and `mask`, as defined in the [quickstart](quickstart.md).

## Images

Every function that takes an image accepts an `ANTsImage` (anything with `numpy()`, `spacing`,
`origin` and `direction`) or a tuple `(array, spacing, origin, direction)` in ITK (LPS)
geometry, where `array[i, j, k]` is the voxel at ITK index `(i, j, k)`. Arrays must be 3-D.
`float32` and `float64` arrays are passed to the engine; other dtypes are converted to
`float32`. Non-finite voxels are refused. A `float32` array in x-fastest (Fortran) order is
passed without a copy.

Reference grids (for `compose_field` and `apply_transforms`) are an `ANTsImage` or a tuple
`(shape, spacing, origin, direction)`.

## Full normalization

`register_syn(fixed, moving, out_prefix, *, iso_mm=1.0, write_warped=False, adapter=None,
cancel=None, recipe=None)` runs Rigid and Affine (Mattes MI on the first channel), then SyN (CC
on every channel), from a centre-of-mass start, on an isotropic grid of `iso_mm` built from the
fixed image.

`fixed` and `moving` are one image, or a list of two (T1w + T2w) for the multispectral SyN
stage. The moving T2w must already be in the moving T1w's space. The call releases the GIL
while it runs.

### Output files

`out_prefix` is not a folder. The engine appends the file names to it as text, so the prefix
`out/sub_` gives these files:

| File | Content |
| --- | --- |
| `out/sub_Composite.h5` | `[Affine, Warp]`, forward |
| `out/sub_InverseComposite.h5` | `[InverseWarp, Affine^-1]` |
| `out/sub_0GenericAffine.mat` | the collapsed affine |
| `out/sub__Warped.nii.gz` | the warped moving image, only with `write_warped=True` (two underscores: the prefix ends in one) |

The folder of the prefix is created when it is missing. A prefix without a trailing separator
or underscore, such as `out/sub`, gives `out/subComposite.h5`.

### Result

`register_syn` returns a dict with these keys:

| Key | Content |
| --- | --- |
| `adapter` | name of the GPU used |
| `grid` | size of the registration grid, in voxels |
| `total_s` | total time, including preparing and uploading the images |
| `resample_s`, `preprocess_s`, `linear_s`, `syn_s`, `warp_s`, `write_s` | time of each step |
| `levels` | per SyN level: level, grid size, number of iterations, whether it converged, last energy |
| `affine_parameters`, `affine_center` | the collapsed linear transform (12 parameters, centre) |
| `peak_gpu_bytes` | peak GPU memory the run allocated |
| `memory_budget_bytes` | the memory budget of the capacity check |
| `recipe` | the full recipe that ran |

TODO(author): give the types and units of the entries of `levels`, and say when
`affine_parameters` is `None`. The source builds `levels` as tuples and `affine_parameters` as
an optional value.

### Cancellation

```python
import threading

import fastwarp

token = fastwarp.CancelToken()
outcome = {}


def run():
    try:
        outcome["info"] = fastwarp.register_syn(fixed, moving, "out/sub_", cancel=token)
    except fastwarp.RegistrationCancelled:
        outcome["cancelled"] = True


worker = threading.Thread(target=run)
worker.start()
token.cancel()
worker.join()
print(outcome)
```

The run stops at the next iteration and raises `RegistrationCancelled`, which `run` catches.
Cancelling before the run starts also raises it.

## Recipes

The schedule is the caller's. `cohort_recipe(channels)` returns the default recipe in full, and
`register_syn(..., recipe=...)` takes the same structure with any stage or field left out
(it then keeps the default). This example halves the SyN iterations and uses a CC radius of 2:

```python
recipe = fastwarp.cohort_recipe(channels=1)
recipe["syn"]["iterations"] = [50, 35, 25, 10]
recipe["syn"]["cc_radius"] = 2
info = fastwarp.register_syn(fixed, moving, "out/sub_", recipe=recipe)
print(info["recipe"]["syn"]["iterations"])
```

A partial recipe is enough: `recipe={"syn": {"iterations": [50, 35, 25, 10]}}`. The number of
entries in `iterations` must match the number of levels (`shrink_factors`, `smoothing_sigmas`).

- `rigid` and `affine`: Mattes MI over the first channel with the fields `gradient_step`,
  `iterations`, `convergence_threshold`, `convergence_window`, `shrink_factors`,
  `smoothing_sigmas`, `sigma_units` (`"vox"` or `"mm"`), `bins`, `sampling_percentage` and
  `seed`. Sampling is regular.
- `syn`: CC over every channel with `gradient_step`, `update_field_variance`,
  `total_field_variance`, `cc_radius`, `channel_weights` and the same schedule fields.
- Images are winsorized to `[0.005, 0.995]`; there is no histogram matching.

The default is Rigid and Affine with `MI[32 bins, regular 25 %]` at `1000x500x250x100`, and SyN
`[0.1, 3, 0]` with `CC[radius 4]` (T1w + T2w: weight 0.5 each) at `100x70x50x20`, shrink factors
`8x4x2x1` and smoothing `3x2x1x0` voxels.

An unknown key, a value of the wrong type, mismatched level counts, or a value the GPU kernels
do not support (MI bins other than 32, a CC radius above 8) raises `ValueError` before any work.

## Linear sessions

`linear_session(fixed, moving, *, fixed_mask=None, winsorize=None, adapter=None)` returns a
`LinearSession`. The pair is converted, winsorized and uploaded once, and the smoothed copies
are kept between fits. Use it for a multi-start, or for a rigid fit followed by an affine one.

```python
with fastwarp.linear_session(fixed, moving, fixed_mask=mask) as session:
    rigid = session.register(transform="rigid")
    affine = session.register(
        transform="affine", initial=(rigid["parameters"], rigid["center"])
    )
print(affine["metric"])
```

`session.register(stage=None, initial=None, use_mask=True, out_prefix=None, cancel=None, *,
transform="rigid", mask=None)`:

- `transform` is `"rigid"` or `"affine"`. `stage` holds the fields of `cohort_recipe()["rigid"]`.
- `initial` is `(parameters, center)` of an ITK affine that maps fixed points to moving points,
  with 12 parameters (the row-major matrix, then the translation). `None` starts from the
  centres of intensity mass, like `--initial-moving-transform [f,m,1]`. The fitted transform
  starts as the identity, centred where `initial` is, as an ANTs stage does after
  `--initial-moving-transform`. `session.com_initial` returns the centre-of-mass start.
- `mask` is a fixed-image mask for this fit; `None` uses the session's `fixed_mask`. ANTs'
  moving-image mask has no equivalent.
- `out_prefix` writes `<out_prefix>0GenericAffine.mat`.

The result dict has these keys: `adapter`, `parameters` and `center` (the collapsed affine,
start then fit, what `0GenericAffine.mat` holds), `initial_parameters` and `initial_center`
(the start), `metric` (the final MI value, lower is better; `None` when the last level ran no
iteration), `iterations` (per level), `total_s`, `transform` (`"rigid"` or `"affine"`) and
`stage` (the schedule that ran).

`register_rigid(fixed, moving, *, stage=None, initial=None, fixed_mask=None, winsorize=None,
out_prefix=None, adapter=None, cancel=None)` makes one rigid fit in one call. It checks every
argument before it opens the GPU. `rigid_session` and `RigidSession` are the 0.3 names of
`linear_session` and `LinearSession`.

## Post-processing and resampling

Both functions run on all CPU cores and need no GPU. They equal ANTs to `float32` rounding
(`tests/test_post.py`).

- `compose_field(transforms, reference, out_path, *, compression_level=None)` writes the
  transforms as one displacement field on the reference grid: the file
  `antsApplyTransforms --compose` writes (`float32` vectors in LPS mm). With `.nii.gz` it uses
  parallel gzip. `compression_level` is 0 to 9; 0 stores the data as valid gzip without
  compressing it, which suits displacement fields. It returns the output path.
- `apply_transforms(moving, reference, transforms, *, interpolation="linear",
  default_value=0.0)` resamples like `antsApplyTransforms`. `interpolation` is one of
  `fastwarp.INTERPOLATORS`: `linear`, `nearestNeighbor`, `lanczosWindowedSinc`. It returns an
  `ANTsImage` like the reference, or a `float32` array when the reference is a tuple. Points
  that map outside `moving` take `default_value`.

`transforms` is a list as `ants.apply_transforms` takes it, with the same meaning: a reference
point goes through the first element first.

## GPU checks and fallbacks

| Function | Use |
| --- | --- |
| `gpu_available(adapter=None)` | `True` when a usable hardware GPU opens |
| `gpu_info(adapter=None)` | describes the GPU, or raises `GpuUnavailableError` |
| `fits_gpu(fixed, *, iso_mm=1.0, channels=1, adapter=None)` | whether `register_syn` at `iso_mm` fits; `False` without a GPU |
| `iso_grid_dims(img, iso_mm=1.0)` | voxels per axis of the registration grid |

`GpuUnavailableError` and `GpuCapacityError` are raised before any work, so a caller can fall
back to ANTs. `RegistrationError` covers other failures, including an internal engine panic,
which is raised as `RegistrationError` and not as `PanicException`. `RegistrationCancelled`
derives from `Exception`; the others derive from `RuntimeError`.

The capacity check uses the device memory the graphics API reports, halved on integrated GPUs
because they share memory with the host. Software adapters are refused unless
`FASTWARP_GPU_ALLOW_SOFTWARE=1`. The variables are listed under
[Installation](installation.md#environment-variables).

## How ANTs tools read the outputs

Pass the composite to `ants.apply_transforms` or `antsApplyTransforms` as a transform list, as
for a registration made by ANTs. `tests/test_register.py` (`test_ants_reads_the_transforms`)
warps the moving image with `Composite.h5` and back with `InverseComposite.h5`.

TODO(author): document which direction of `antsApplyTransforms` each file is meant for, with a
command line example.
