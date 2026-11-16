# Resampling

Two functions apply ANTs transform files, like `antsApplyTransforms`. They run in parallel on
all CPU cores and need no GPU, so they also work on a machine where the registration itself ran
with ANTs.

## Transform lists

Both functions take `transforms`: one file path, or a list of paths, with the same meaning as
the `transformlist` of {func}`ants.apply_transforms` and the `-t` options of
`antsApplyTransforms`. A point of the reference grid goes through the first file of the list
first, then through the second, and so on. To warp a subject's scan into template space with
separate ANTs files, list them as ANTs writes them: `["sub_1Warp.nii.gz",
"sub_0GenericAffine.mat"]`.

These files can be read:

| Extension | Content |
| --- | --- |
| `.h5`, `.hdf5`, `.hdf` | An ITK HDF5 transform file, for example `Composite.h5`. |
| `.mat` | An ITK MATLAB affine file, for example `0GenericAffine.mat`. |
| `.txt`, `.tfm` | An ITK text transform file with a linear transform. |
| `.nii`, `.nii.gz` | A displacement field, for example `1Warp.nii.gz` or the output of {func}`~fastwarp.compose_field`. |

`invert` uses the inverse of a file in its place, as `antsApplyTransforms -t [file,1]` does and
as ANTsPy's `whichtoinvert`: a list with one `bool` per file, `True` for the files to invert
(a tuple or a 1-D boolean NumPy array works too; `0` and `1` are refused). `None`, the default,
inverts nothing. Unlike ANTsPy, FastWarp never inverts a file on its own; ANTsPy inverts the
first of two files when only that one is a `.mat`. Only linear transforms can be inverted: `.mat`
and `.txt` files and composites that hold only affines. Inverting a displacement field, or a
composite that holds one, raises `ValueError`; pass the matching inverse file instead, such as
`InverseComposite.h5` or `1InverseWarp.nii.gz`.

A missing file raises `FileNotFoundError` before any work.

## Resample an image

{func}`~fastwarp.apply_transforms` resamples an image. All parameters:
{func}`fastwarp.apply_transforms` in the API reference.

| Argument | Default | Meaning |
| --- | --- | --- |
| `moving` | required | The [image](images.md) to resample. |
| `reference` | required | The output grid: an `ANTsImage` or a tuple `(shape, spacing, origin, direction)`. |
| `transforms` | required | A file or a list of files (see [Transform lists](#transform-lists)). |
| `interpolation` | `"linear"` | One of {data}`~fastwarp.INTERPOLATORS`: `"linear"`, `"nearestNeighbor"` (for label images) or `"lanczosWindowedSinc"`. |
| `default_value` | `0.0` | The value of output voxels that map outside `moving`. |
| `invert` | `None` | One `bool` per transform file, `True` to use that file's inverse (see above). |

The result is an `ANTsImage` like `reference` when `reference` is an `ANTsImage`, and otherwise a
`float32` NumPy array of the reference's shape.

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))
grid = (fixed.shape, fixed.spacing, fixed.origin, fixed.direction)

image = fastwarp.apply_transforms(
    moving, fixed, ["out/ch2_to_mni_Composite.h5"], interpolation="lanczosWindowedSinc"
)
array = fastwarp.apply_transforms(moving, grid, ["out/ch2_to_mni_Composite.h5"])
print(type(image).__name__, type(array).__name__, array.dtype, array.shape)
```

```{code-block} text
:class: fw-output
ANTsImage ndarray float32 (182, 218, 182)
```

The transform file comes from the [quickstart](../quickstart.md) example.

## Write one displacement field

{func}`~fastwarp.compose_field` evaluates the whole transform list at every voxel of the
reference grid and writes the result as one {term}`displacement field`. It is the file that
`antsApplyTransforms -o [field.nii.gz,1]` writes: a NIfTI vector image of `float32` displacements
in {term}`LPS` millimetres, which ANTs and FastWarp read back as a single transform. All
parameters: {func}`fastwarp.compose_field` in the API reference.

| Argument | Default | Meaning |
| --- | --- | --- |
| `transforms` | required | A file or a list of files (see [Transform lists](#transform-lists)). |
| `reference` | required | The output grid: an `ANTsImage` or a tuple `(shape, spacing, origin, direction)`. |
| `out_path` | required | The output file. `.nii.gz` is written with gzip compression on several threads; `.nii` uncompressed. |
| `compression_level` | `None` | The gzip level from 0 to 9; `None` uses level 1. Level 0 writes a valid gzip file without compressing the data, which suits displacement fields: they barely compress, and the file is written and read faster. |
| `invert` | `None` | One `bool` per transform file, as for `apply_transforms`. |

It returns the output path as a {class}`pathlib.Path`. The transform files are read and checked
first; the output folder is created, when it is missing, only after the field has been
computed.

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
path = fastwarp.compose_field(
    ["out/ch2_to_mni_Composite.h5"], fixed, "out/ch2_to_mni_field.nii.gz", compression_level=0
)
field = ants.image_read(str(path))
print(field.shape, field.components)
```

```{code-block} text
:class: fw-output
(182, 218, 182) 3
```

## Agreement with ANTs

The tests compare both functions with ANTsPy on the same transforms (`tests/test_post.py`). For
every interpolator, the resampled images may differ by at most 0.001 in absolute value on test
images with values up to about 110, and the composed displacement fields by at most 0.0001 mm.
Running the commands of [Output files](outputs.md#with-antsapplytransforms) with
`antsApplyTransforms` 2.6.5 on the quickstart's transforms gave the same resampled image as
{func}`~fastwarp.apply_transforms` (largest difference 0) and a displacement field that differs
from {func}`~fastwarp.compose_field`'s by at most 6 × 10⁻⁸ mm, on one Windows laptop.
