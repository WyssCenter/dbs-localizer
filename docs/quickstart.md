# Quickstart

## End-to-end example on public data

This example uses the 3-D `mni` sample that ANTsPy ships. It needs `antspyx` and a GPU. It
shifts the image by a known translation, registers the shifted copy back onto the original and
reads the result with ANTs. TODO(author): this example has not been run by the documentation
author; run it once and confirm the sample's voxel size and the printed values.

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))

shift = ants.create_ants_transform(
    transform_type="AffineTransform",
    dimension=3,
    translation=(4.0, -3.0, 2.0),
)
ants.write_transform(shift, "out/shift.mat")
moving = ants.apply_transforms(fixed=fixed, moving=fixed, transformlist=["out/shift.mat"])

if not fastwarp.fits_gpu(fixed, iso_mm=2.0):
    raise SystemExit("no GPU, or too little GPU memory: use ANTs")

info = fastwarp.register_syn(fixed, moving, "out/mni_", iso_mm=2.0)
print(info["adapter"], f'{info["total_s"]:.1f} s')

warped = ants.apply_transforms(
    fixed=fixed, moving=moving, transformlist=["out/mni_Composite.h5"]
)
print(ants.image_similarity(fixed, warped, metric_type="MeanSquares"))
```

The images are passed as `ANTsImage` objects, as the tests do (`tests/test_register.py` builds
them with `ants.from_numpy`). The `out/` folder must exist before `ants.write_transform`;
`register_syn` creates the folder of its own output prefix.

## Your own images

```python
import ants
import fastwarp

fixed = [ants.image_read("tpl_t1.nii"), ants.image_read("tpl_t2.nii")]
moving = [ants.image_read("sub_T1w.nii.gz"), ants.image_read("sub_T2w_in_T1w.nii.gz")]

if fastwarp.fits_gpu(fixed[0], iso_mm=1.0, channels=2):
    info = fastwarp.register_syn(fixed, moving, "out/sub_", iso_mm=1.0)
    warped = ants.apply_transforms(fixed[0], moving[0], ["out/sub_Composite.h5"])
    print(info["adapter"], f'{info["total_s"]:.1f} s')
else:
    print("no GPU or too little GPU memory: use ANTs")
```

The file names are placeholders for your template and subject images. The moving T2w must
already be in the moving T1w's space. `register_syn` writes `out/sub_Composite.h5`,
`out/sub_InverseComposite.h5` and `out/sub_0GenericAffine.mat`.

## A rigid fit

```python
import ants
import fastwarp

fixed = ants.image_read("anchor_T1w.nii.gz")
moving = ants.image_read("post_op_CT.nii.gz")

result = fastwarp.register_rigid(fixed, moving, out_prefix="out/coreg_")
print(result["parameters"], result["center"], result["metric"])
```

## Resample the result

```python
import ants
import fastwarp

reference = ants.image_read("tpl_t1.nii")
moving = ants.image_read("sub_T1w.nii.gz")

fastwarp.compose_field(["out/sub_Composite.h5"], reference, "out/sub_field.nii.gz")
resampled = fastwarp.apply_transforms(moving, reference, ["out/sub_Composite.h5"])
```

Neither function needs a GPU. See the [user guide](user-guide.md) for the details.
