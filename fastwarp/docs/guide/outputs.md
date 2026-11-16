# Output files

By default ({func}`~fastwarp.register_syn` with `outputs="composite"`) FastWarp writes the two
{term}`composite <composite transform>` files that `antsRegistration --write-composite-transform 1`
writes, plus the affine part as `0GenericAffine.mat`. With `outputs="split"` it writes ANTs'
separate files instead, and with `"both"` all of them (see [Split files](#split-files)). Tools
that read ANTs transforms read them: `antsApplyTransforms`,
`antsApplyTransformsToPoints`, [ANTsPy](https://github.com/ANTsX/ANTsPy), and FastWarp's own
[resampling functions](resampling.md). A {class}`~fastwarp.LinearSession` fit writes only the
affine file.

## Which file to use

Below, `out/sub_` stands for the `out_prefix` you passed, *fixed* for the image you registered
onto (for example a template) and *moving* for the image you registered (for example a
subject's scan).

::::{grid} 1 1 2 2
:gutter: 2
:class-container: fw-file-cards

:::{grid-item-card} `out/sub_Composite.h5`
:class-card: sd-shadow-none

**Use it to** bring a subject's scan into template space.

- Warps images from moving space onto the fixed grid.
- Maps points from fixed space to moving space.
:::

:::{grid-item-card} `out/sub_InverseComposite.h5`
:class-card: sd-shadow-none

**Use it to** bring a template or an atlas into the subject's space, or to move points
measured in the subject's scan into template space.

- Warps images from fixed space onto the moving grid.
- Maps points from moving space to fixed space.
:::

:::{grid-item-card} `out/sub_0GenericAffine.mat`
:class-card: sd-shadow-none

**Use it to** apply only the linear part, in either direction (`[file,1]` inverts it in
`antsApplyTransforms`, `invert=[True]` in FastWarp).

- The affine part of `Composite.h5`, in the same direction.
:::

:::{grid-item-card} `out/sub__Warped.nii.gz`
:class-card: sd-shadow-none

**Use it to** look at the result. Written only with `write_warped=True`.

- The first moving image on the registration grid.
:::
::::

Points and images go in opposite directions:

```{raw} html
<svg class="fw-diagram" viewBox="0 0 480 290" role="img" aria-labelledby="fw-dir-title fw-dir-desc">
  <title id="fw-dir-title">Which direction each composite maps</title>
  <desc id="fw-dir-desc">Composite.h5 maps points from fixed space to moving space and warps images from moving space to fixed space. InverseComposite.h5 maps points from moving space to fixed space and warps images from fixed space to moving space.</desc>
  <defs>
    <marker id="fw-arrow-p" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse"><path class="fw-head-points" d="M0,0 L10,5 L0,10 z"/></marker>
    <marker id="fw-arrow-i" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse"><path class="fw-head-images" d="M0,0 L10,5 L0,10 z"/></marker>
  </defs>
  <rect class="fw-box" x="2" y="10" width="104" height="230" rx="10"/>
  <text class="fw-title" x="54" y="112" text-anchor="middle">Fixed</text>
  <text class="fw-title" x="54" y="136" text-anchor="middle">space</text>
  <text class="fw-sub" x="54" y="162" text-anchor="middle">template</text>
  <rect class="fw-box" x="374" y="10" width="104" height="230" rx="10"/>
  <text class="fw-title" x="426" y="112" text-anchor="middle">Moving</text>
  <text class="fw-title" x="426" y="136" text-anchor="middle">space</text>
  <text class="fw-sub" x="426" y="162" text-anchor="middle">subject</text>
  <text class="fw-label" x="240" y="36" text-anchor="middle">Composite.h5</text>
  <line class="fw-points" x1="114" y1="56" x2="364" y2="56" marker-end="url(#fw-arrow-p)"/>
  <line class="fw-images" x1="364" y1="88" x2="114" y2="88" marker-end="url(#fw-arrow-i)"/>
  <text class="fw-label" x="240" y="152" text-anchor="middle">InverseComposite.h5</text>
  <line class="fw-points" x1="364" y1="172" x2="114" y2="172" marker-end="url(#fw-arrow-p)"/>
  <line class="fw-images" x1="114" y1="204" x2="364" y2="204" marker-end="url(#fw-arrow-i)"/>
  <line class="fw-points" x1="110" y1="274" x2="160" y2="274"/>
  <text class="fw-sub" x="168" y="280">points</text>
  <line class="fw-images" x1="262" y1="274" x2="312" y2="274"/>
  <text class="fw-sub" x="320" y="280">images</text>
</svg>
```

This is the ANTs convention. To fill a voxel of the output grid, a resampler looks up where that
voxel's centre lies in the input image, so the transform that warps an image from moving to
fixed space maps points from fixed to moving space. To move a point from the subject's scan into
template space, use the inverse composite.

## With antsApplyTransforms

The commands below use `sub.nii.gz` for the subject's scan and `tpl.nii.gz` for the template.

A subject's scan in template space:

```bash
antsApplyTransforms -d 3 \
  -i sub.nii.gz -r tpl.nii.gz \
  -t out/sub_Composite.h5 \
  -o sub_in_tpl.nii.gz
```

Atlas labels in the subject's space, with nearest-neighbour interpolation so that the labels
stay whole numbers:

```bash
antsApplyTransforms -d 3 \
  -i labels.nii.gz -r sub.nii.gz \
  -t out/sub_InverseComposite.h5 \
  -n NearestNeighbor \
  -o labels_in_sub.nii.gz
```

The affine part alone, forward and inverse (`[file,1]` inverts an affine file):

```bash
antsApplyTransforms -d 3 \
  -i sub.nii.gz -r tpl.nii.gz \
  -t out/sub_0GenericAffine.mat \
  -o sub_affine.nii.gz
antsApplyTransforms -d 3 \
  -i tpl.nii.gz -r sub.nii.gz \
  -t "[out/sub_0GenericAffine.mat,1]" \
  -o tpl_affine.nii.gz
```

The forward transform as one {term}`displacement field` on the template grid
({func}`~fastwarp.compose_field` writes the same file):

```bash
antsApplyTransforms -d 3 \
  -r tpl.nii.gz \
  -t out/sub_Composite.h5 \
  -o "[field.nii.gz,1]"
```

Points measured in the subject's scan, for example electrode contacts, in template space. The
CSV file has the columns `x,y,z,t` in {term}`LPS` millimetres:

```bash
antsApplyTransformsToPoints -d 3 \
  -i points_sub.csv \
  -o points_tpl.csv \
  -t out/sub_InverseComposite.h5
```

## With ANTsPy

The points are a pandas table, which comes with ANTsPy (pandas is one of its dependencies).

```python
import ants
import pandas as pd

template = ants.image_read("tpl.nii.gz")
sub = ants.image_read("sub.nii.gz")
inverse = ["out/sub_InverseComposite.h5"]

sub_in_template = ants.apply_transforms(template, sub, ["out/sub_Composite.h5"])
template_in_sub = ants.apply_transforms(sub, template, inverse)

points = pd.DataFrame({"x": [10.0], "y": [-20.0], "z": [5.0]})
points_in_template = ants.apply_transforms_to_points(3, points, inverse)
```

## With FastWarp

{func}`~fastwarp.apply_transforms` takes the same transform lists as `ants.apply_transforms`,
with the image to resample first and the reference grid second:

```python
import ants
import fastwarp

template = ants.image_read("tpl.nii.gz")
sub = ants.image_read("sub.nii.gz")

sub_in_template = fastwarp.apply_transforms(sub, template, ["out/sub_Composite.h5"])
template_in_sub = fastwarp.apply_transforms(template, sub, ["out/sub_InverseComposite.h5"])
```

`invert`, one flag per file, uses the inverse of an affine file in its place, as `[file,1]` does
in `antsApplyTransforms` and `whichtoinvert` in ANTsPy. Only linear transforms can be inverted;
for a displacement field, pass the matching inverse file (see
[Resampling](resampling.md#transform-lists)).

## Split files

`outputs="split"` writes what `antsRegistration` writes without `--write-composite-transform`:

| File | Content |
| --- | --- |
| `out/sub_0GenericAffine.mat` | The affine part. |
| `out/sub_1Warp.nii.gz` | The SyN displacement field, forward; only when a SyN stage ran. |
| `out/sub_1InverseWarp.nii.gz` | The SyN displacement field, inverse; only when a SyN stage ran. |

`outputs="both"` writes these and the composites: five files after a SyN stage, three without
one. The names are those of `--collapse-output-transforms 1`, ANTs' default; ANTs' per-stage
files (`-z 0`) are not written. The fields are stored as `float32`, where ANTs writes `float64`.

With split files the warp and the affine are listed separately, as ANTs users know them. A
subject's scan in template space, and the template in the subject's space:

```python
import ants
import fastwarp

template = ants.image_read("tpl.nii.gz")
sub = ants.image_read("sub.nii.gz")

sub_in_template = fastwarp.apply_transforms(
    sub, template, ["out/sub_1Warp.nii.gz", "out/sub_0GenericAffine.mat"]
)
template_in_sub = fastwarp.apply_transforms(
    template, sub, ["out/sub_0GenericAffine.mat", "out/sub_1InverseWarp.nii.gz"],
    invert=[True, False],
)
```

The same with `antsApplyTransforms`:

```bash
antsApplyTransforms -d 3   -i sub.nii.gz -r tpl.nii.gz   -t out/sub_1Warp.nii.gz   -t out/sub_0GenericAffine.mat   -o sub_in_tpl_split.nii.gz
antsApplyTransforms -d 3   -i tpl.nii.gz -r sub.nii.gz   -t "[out/sub_0GenericAffine.mat,1]"   -t out/sub_1InverseWarp.nii.gz   -o tpl_in_sub_split.nii.gz
```

## What the files contain

`Composite.h5`
: An ITK HDF5 transform file with two transforms, applied to a point of the fixed space in this
  order: the SyN displacement field, then the affine. ANTs lists them as `[Affine, Warp]`. The
  displacement field lies on the registration grid (see
  [Normalization](normalization.md#the-registration-grid)). The data are stored in double
  precision, without compression.

`InverseComposite.h5`
: The inverse, `[InverseWarp, Affine⁻¹]` in ANTs' notation.

Without a SyN stage, `Composite.h5` holds only `[Affine]` and `InverseComposite.h5` only
`[Affine⁻¹]`, as ANTs 2.6.5 writes them for a linear-only run.

`0GenericAffine.mat`
: An ITK MATLAB transform file with one `AffineTransform_double_3_3`: the rigid and affine stages
  combined into one affine, as ANTs writes it with `--collapse-output-transforms 1`. Its 12
  parameters and centre are also in the result as `affine_parameters` and `affine_center`. A
  SyN-only run from `"identity"` writes the identity here, as `antsRegistration -r Identity`
  does.

`1Warp.nii.gz`, `1InverseWarp.nii.gz`
: The SyN displacement fields of the split layout, as NIfTI vector images of `float32`
  displacements in LPS millimetres, on the registration grid.

`_Warped.nii.gz`
: The first moving image warped with linear interpolation onto the registration grid, as
  `float32`. It lies on the registration grid, not on the fixed image's own grid; for an image on
  the fixed grid, resample with `Composite.h5`.

The files are tested against ANTs: `tests/test_register.py::test_ants_reads_the_transforms`
warps with both composites through ANTsPy, and the Rust tests in `crates/fastwarp-reg` compare
the engine's transform files with the ANTs 2.6.5 programs (see [Testing](../testing.md)).
