# Linear fits

{func}`~fastwarp.linear_session` aligns one image pair with {term}`rigid` or {term}`affine`
transforms, for example in a {term}`coregistration` of a CT scan onto an MRI scan of the same
patient. It returns a {class}`~fastwarp.LinearSession`: the two images are prepared once, and
their smoothed copies are kept between fits. Several fits of the
same pair, such as a search from several starting positions, or a rigid fit followed by an
affine one, then pay for that preparation only once. All parameters:
{func}`fastwarp.linear_session` in the API reference.

| Argument | Default | Meaning |
| --- | --- | --- |
| `fixed`, `moving` | required | One [image](images.md) each. |
| `fixed_mask` | `None` | An image whose non-zero voxels mark where the metric samples the fixed image, in every fit that asks for a mask (see [Masks](#masks)). |
| `winsorize` | `None` | `(lower, upper)` quantiles, as `antsRegistration --winsorize-image-intensities [lower,upper]`: intensities are clipped to these quantiles and rescaled to [0, 1]. `None` leaves the intensities as they are. |
| `adapter` | `None` | Use the GPU whose name contains this text. |

Unlike {func}`~fastwarp.register_syn`, a session registers the images on their own grids,
without resampling them to an isotropic grid, and does not winsorize unless you ask for it.

Use the session in a `with` block, or call {meth}`~fastwarp.LinearSession.close`, to free the
GPU and the kept images. A closed session raises `ValueError` when it is used again.

## One fit

{meth}`~fastwarp.LinearSession.register` runs one `Rigid` or `Affine` stage. All parameters:
{meth}`fastwarp.LinearSession.register` in the API reference.

| Argument | Default | Meaning |
| --- | --- | --- |
| `stage` | `None` | The schedule: the fields of a linear [recipe stage](recipes.md#rigid-and-affine-stages). Fields left out keep their defaults. |
| `initial` | `None` | The start, as `(parameters, center)` of an affine (see below). `None` starts from the {term}`centres of mass <centre of mass>`. |
| `use_mask` | `True` | Sample only inside the mask. With `False`, the fit ignores `fixed_mask`. |
| `out_prefix` | `None` | Write the result as `<out_prefix>0GenericAffine.mat`. The [prefix rules](normalization.md#output-prefix) of `register_syn` apply. |
| `cancel` | `None` | A {class}`~fastwarp.CancelToken`. |
| `transform` | `"rigid"` | `"rigid"` (rotation and translation) or `"affine"` (also scaling and shearing). |
| `mask` | `None` | A mask for this fit only, in place of the session's `fixed_mask`. |

An affine is given as `(parameters, center)`, in the form ITK uses: 12 parameters (the 3 × 3
matrix row by row, then the translation) and the centre of rotation (3 values, LPS
millimetres). It maps points of the fixed image to points of the moving image. The fitted
transform starts as the identity, centred where `initial` is, and is then combined with
`initial`, as an ANTs stage does after `--initial-moving-transform`.
{attr}`~fastwarp.LinearSession.com_initial` is the centre-of-mass start that `initial=None` uses.

The fit returns a dictionary:

| Key | Type | Content |
| --- | --- | --- |
| `parameters`, `center` | `list[float]` | The result: start and fit combined into one affine, what `0GenericAffine.mat` holds. |
| `initial_parameters`, `initial_center` | `list[float]` | The start that was used. |
| `metric` | `float` or `None` | The final value of the metric, the negative mutual information (lower is better). `None` when the last level ran no iteration. |
| `iterations` | `list[int]` | Iterations run per level. |
| `total_s` | `float` | Seconds the fit took. |
| `transform` | `str` | `"rigid"` or `"affine"`. |
| `stage` | `dict` | The full schedule that ran. |
| `adapter` | `str` | The GPU used. |
| `warnings` | `list[str]` | One entry per level skipped because its sample points leave mutual information undefined; each is also emitted as a `UserWarning`. Empty when every level ran. |

## A rigid fit, then an affine one

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))

with fastwarp.linear_session(fixed, moving, winsorize=(0.005, 0.995)) as session:
    rigid = session.register(transform="rigid")
    affine = session.register(
        transform="affine",
        initial=(rigid["parameters"], rigid["center"]),
        out_prefix="out/ch2_affine_",
    )

print(rigid["metric"], affine["metric"])
```

## A search from several starts

A short fit from several starting rotations, then a full fit from the best one. Each start is
the centre-of-mass start turned about the z axis:

```python
import math

import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))
coarse = {"iterations": [1000, 500], "shrink_factors": [8, 4], "smoothing_sigmas": [3.0, 2.0]}

with fastwarp.linear_session(fixed, moving) as session:
    params, center = session.com_initial
    fits = []
    for degrees in (-20, -10, 0, 10, 20):
        a = math.radians(degrees)
        rotation = [math.cos(a), -math.sin(a), 0.0, math.sin(a), math.cos(a), 0.0, 0.0, 0.0, 1.0]
        start = (rotation + list(params[9:]), center)
        fits.append(session.register(stage=coarse, initial=start))
    best = min(fits, key=lambda fit: fit["metric"])
    final = session.register(initial=(best["parameters"], best["center"]))

print(f"best coarse metric {best['metric']:.4f}, final {final['metric']:.4f}")
```

## Masks

A mask limits the samples of the metric to part of the fixed image, as
`antsRegistration --masks [fixed_mask,NULL]` does. It is an image on any grid; its non-zero
voxels are inside. There is no mask for the moving image.

- `fixed_mask` on the session applies to every fit with `use_mask=True` (the default) that
  brings no `mask` of its own.
- `mask` on one fit replaces the session's mask for that fit.
- `use_mask=False` turns masking off for one fit. Passing a `mask` together with
  `use_mask=False` raises `ValueError`.

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))
brain = ants.get_mask(fixed)

with fastwarp.linear_session(fixed, moving, fixed_mask=brain) as session:
    masked = session.register()
    unmasked = session.register(use_mask=False)
```

## One session, one fit at a time

A session runs one fit at a time. A second thread that calls
{meth}`~fastwarp.LinearSession.register` on the same session while a fit runs gets
`ValueError: the session is closed or busy`. Each fit releases Python's global interpreter lock
while it runs.

## Deprecated names

`register_rigid`, `rigid_session` and `RigidSession` still work but warn with a
`DeprecationWarning` and will be removed in FastWarp 0.3.0. Replace them as follows:

| Deprecated | Use |
| --- | --- |
| `fastwarp.register_rigid(fixed, moving, ...)` | `fastwarp.linear_session(fixed, moving, ...).register(...)`, in a `with` block |
| `fastwarp.rigid_session(...)` | `fastwarp.linear_session(...)` |
| `fastwarp.RigidSession` | `fastwarp.LinearSession` |
