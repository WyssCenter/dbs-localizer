# Images

The Python functions do not read image files: you pass images that are already in memory, in one
of the two forms below. To work on NIfTI files without Python code, use the
[command line](../cli.md), which reads them as ITK and ANTsPy do.

## Two ways to pass an image

`ANTsImage`
: An image from [ANTsPy](https://github.com/ANTsX/ANTsPy), for example from
  {func}`ants.image_read`. Any object with a `numpy()` method and `shape`, `spacing`, `origin`
  and `direction` attributes works the same way.

Tuple
: `(array, spacing, origin, direction)`, with a 3-D NumPy array, three spacings in millimetres,
  the physical position of voxel `(0, 0, 0)` in millimetres, and a 3 × 3 direction matrix.

Both forms describe the image in the geometry of [ITK](https://itk.org), which ANTs uses:

- `array[i, j, k]` is the voxel at index `(i, j, k)`, the first index running along the first
  image axis. This is the order `ANTsImage.numpy()` returns.
- The physical point of index `(i, j, k)` is `origin + direction @ (spacing * (i, j, k))`, in
  LPS coordinates: x grows towards the patient's left, y towards posterior, z towards superior.
- Column `a` of `direction` is the unit vector of image axis `a`.

## Data types

- The array must be 3-D.
- `float32` and `float64` arrays are passed to the engine as they are; arrays of any other type
  are converted to `float32` first.
- A `float32` or `float64` array in Fortran order (the first index changing fastest), which is
  what ANTsPy returns, reaches the engine without an extra copy. Other arrays are first copied
  into that layout. The engine then converts every image to `float64` once.
- Voxels that are NaN or infinite make the registration fail with
  {class}`~fastwarp.RegistrationError`.

## Several channels

{func}`~fastwarp.register_syn` accepts one image or a list of two images on each side, for
example a T1-weighted and a T2-weighted scan. The second moving image must already be aligned to
the first one, for example by a rigid fit with {func}`~fastwarp.linear_session`. See
[Normalization](normalization.md#two-channels).

## Masks

A mask is an image as well. Its non-zero voxels mark where the metric samples the fixed image.
It may lie on any grid, as only its physical position matters. Masks are used by the
[linear fits](linear.md#masks).

## Reference grids

{func}`~fastwarp.compose_field` and {func}`~fastwarp.apply_transforms` resample onto a reference
grid. It is an `ANTsImage`, or a tuple `(shape, spacing, origin, direction)` that describes the
grid without data.

## Images read with nibabel

[nibabel](https://nipy.org/nibabel/) gives an image's geometry as a 4 × 4 affine in RAS
coordinates. This function turns a NIfTI file read with nibabel into a FastWarp tuple:

```python
import nibabel as nib
import numpy as np


def from_nibabel(path):
    img = nib.load(path)
    affine = np.diag([-1.0, -1.0, 1.0]) @ img.affine[:3]
    spacing = np.linalg.norm(affine[:, :3], axis=0)
    direction = affine[:, :3] / spacing
    data = np.asarray(img.dataobj, dtype=np.float32)
    return (data, tuple(spacing), tuple(affine[:, 3]), direction)
```

The first line of the conversion flips the x and y axes from RAS to LPS. nibabel and ITK can
choose differently between the `sform` and the `qform` of a NIfTI header when both are set and
disagree. For geometry that matches ANTs exactly, read the file with ANTsPy.
