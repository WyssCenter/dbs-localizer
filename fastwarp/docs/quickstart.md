# Quickstart

The examples on this page are complete scripts. The first three use
[ANTsPy](https://github.com/ANTsX/ANTsPy) and two of its sample images, which ANTsPy downloads
into `~/.antspy` on first use: `mni`, the MNI152 template at 1 mm, and `ch2`, a single-subject
brain at 1 mm. The last one needs only NumPy.

::::{tab-set}
:sync-group: installer

:::{tab-item} pip
:sync: pip

```bash
pip install fastwarp antspyx
```
:::

:::{tab-item} uv
:sync: uv

```bash
uv add fastwarp antspyx
```
:::

::::

ANTsPy 0.6.3, the version this repository tests with, has wheels for Python 3.11 to 3.13 on
Linux x86_64, macOS (Apple silicon and Intel) and Windows. Elsewhere, for example on Linux
aarch64 or with Python 3.14, pip builds it from its source distribution, which needs a C++
compiler.

## Normalize a brain onto a template

{func}`~fastwarp.register_syn` runs the whole registration: a {term}`centre-of-mass <centre of mass>`
start, a Rigid and an Affine stage, then {term}`SyN`. Here `ch2` (the {term}`moving image`) is
registered onto `mni` (the {term}`fixed image`). {func}`~fastwarp.fits_gpu` first checks that
the registration fits the GPU's memory.

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))

if not fastwarp.fits_gpu(fixed, moving, iso_mm=1.0):
    raise SystemExit("No GPU, or too little GPU memory: use ANTs instead.")

info = fastwarp.register_syn(fixed, moving, "out/ch2_to_mni_")
print(f"{info['adapter']}: {info['total_s']:.1f} s on a {info['grid']} grid")

warped = ants.apply_transforms(
    fixed=fixed, moving=moving, transformlist=["out/ch2_to_mni_Composite.h5"]
)
before = ants.resample_image_to_target(moving, fixed)
print("correlation before:", -ants.image_similarity(fixed, before, "Correlation"))
print("correlation after: ", -ants.image_similarity(fixed, warped, "Correlation"))
```

On a laptop with an Intel Arc 140V integrated GPU (Windows, Vulkan) this printed:

```{code-block} text
:class: fw-output
Intel(R) Arc(TM) 140V GPU (16GB) (Vulkan, IntegratedGpu): 15.1 s on a [182, 218, 182] grid
correlation before: 0.9118086099624634
correlation after:  0.9782331585884094
```

The call created the folder `out/` and wrote three files into it:
`out/ch2_to_mni_Composite.h5`, which warps `ch2` onto `mni`; `out/ch2_to_mni_InverseComposite.h5`,
the inverse; and `out/ch2_to_mni_0GenericAffine.mat`, the affine part alone.
[Output files](guide/outputs.md) explains which file to use for what, and how to use them with
`antsApplyTransforms`.

## Resample with FastWarp

{func}`~fastwarp.apply_transforms` does what `antsApplyTransforms` does, in parallel on the CPU,
and {func}`~fastwarp.compose_field` writes a chain of transforms as one displacement field.
Neither needs a GPU. Run this after the example above:

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))

warped = fastwarp.apply_transforms(moving, fixed, ["out/ch2_to_mni_Composite.h5"])
field = fastwarp.compose_field(
    ["out/ch2_to_mni_Composite.h5"], fixed, "out/ch2_to_mni_field.nii.gz"
)
print(type(warped).__name__, warped.shape, field)
```

```{code-block} text
:class: fw-output
ANTsImage (182, 218, 182) out\ch2_to_mni_field.nii.gz
```

The path is printed with the separator of the operating system (here Windows).

## Fit a rigid transform

{func}`~fastwarp.linear_session` prepares an image pair once and runs one or more rigid or affine
fits on it. One rigid fit:

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))

with fastwarp.linear_session(fixed, moving) as session:
    result = session.register(transform="rigid", out_prefix="out/ch2_rigid_")

print(result["metric"], result["iterations"])
```

This writes `out/ch2_rigid_0GenericAffine.mat`. See [Linear fits](guide/linear.md) for
multi-start searches, masks and affine fits.

## Without ANTsPy

Every function also accepts an image as a tuple `(array, spacing, origin, direction)`. This
example builds a synthetic volume, shifts it by a known amount, and recovers the shift with a
rigid fit:

```python
import numpy as np
import fastwarp


def phantom(shift_mm):
    n, spacing = 64, 2.0
    x, y, z = np.meshgrid(*(np.arange(n) * spacing,) * 3, indexing="ij")
    x, y, z = x - shift_mm[0], y - shift_mm[1], z - shift_mm[2]
    blobs = [(50, 60, 64, 14), (80, 70, 60, 10), (64, 40, 80, 8)]
    vol = sum(
        np.exp(-((x - a) ** 2 + (y - b) ** 2 + (z - c) ** 2) / (2 * s**2))
        for a, b, c, s in blobs
    )
    return (vol.astype(np.float32), (spacing,) * 3, (0.0, 0.0, 0.0), np.eye(3))


fixed = phantom((0.0, 0.0, 0.0))
moving = phantom((4.0, -2.0, 3.0))

with fastwarp.linear_session(fixed, moving) as session:
    result = session.register(transform="rigid")

print("translation (mm):", np.round(result["parameters"][9:], 2))
```

```{code-block} text
:class: fw-output
translation (mm): [ 4.   -1.99  3.  ]
```

The fitted transform maps points of the fixed image to points of the moving image, so its
translation is the shift of the moving copy. [Images](guide/images.md) describes the tuple
format.

## Next steps

- [User guide](user-guide.md): every option, in short pages.
- [GPU and memory](guide/gpu.md): which GPU is used, and what fits into its memory.
- [API reference](api.md): every function and its parameters.
- [Troubleshooting](troubleshooting.md): when no GPU is found, or the registration does not fit.
