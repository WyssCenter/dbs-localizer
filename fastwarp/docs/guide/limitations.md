# What FastWarp does not do

FastWarp covers one part of ANTs: the registration recipe of Wyss DBS Localizer and the
resampling around it. If you need something on this list, use ANTs or ANTsPy for that step.

## Images

- **Images in memory, or NIfTI-1 files.** The Python functions take images in memory (see
  [Images](images.md)), for example read with ANTsPy. The [command line](../cli.md) reads and
  writes NIfTI-1 files (`.nii`, `.nii.gz`) only; other formats (`.mha`, `.nrrd`, Analyze,
  NIfTI-2) must be converted first.
- **3-D only.** 2-D images, time series and vector or multi-component images are not supported.
- **One or two channels.** {func}`~fastwarp.register_syn` registers one image, or two images of
  the same subject, per side.

## Registration

- **A GPU is required.** The registration functions do not run on the CPU. Without a usable GPU
  they raise {class}`~fastwarp.GpuUnavailableError` (see [Falling back to ANTs](gpu.md#fall-back-to-ants)).
- **Three transforms, in a fixed order.** {term}`Rigid`, {term}`affine` and {term}`SyN`: any
  number of rigid and affine stages, then at most one SyN stage (see
  [Stage lists](recipes.md#stage-lists)). ANTs' other transforms, such as `Similarity`,
  `Translation`, `BSplineSyN` or `TimeVaryingVelocityField`, and a linear stage after SyN are not
  available.
- **Two metrics, with limited settings.** {term}`Mattes mutual information` with exactly 32
  histogram bins and regularly spaced samples for the linear stages, and
  {term}`cross-correlation` with a window radius from 1 to 8 voxels for SyN. ANTs' other metrics (`MeanSquares`, `GC`, `Demons`,
  random sampling) are not available.
- **Fixed preparation.** {func}`~fastwarp.register_syn` always works on an isotropic
  {term}`registration grid` and never matches histograms. It either
  {term}`winsorizes <winsorize>` the intensities to the quantiles the recipe gives, or only
  rescales them to [0, 1].
- **Collapsed outputs only.** FastWarp writes the combined files of ANTs'
  `--collapse-output-transforms 1`, not the per-stage files of `-z 0`. A SyN-only run without
  a start (ANTs' `0Warp.nii.gz` layout) is not produced: FastWarp always writes a
  `0GenericAffine.mat`.
- **No moving-image mask.** A mask limits where the fixed image is sampled, as ANTs'
  `--masks [fixed,NULL]` does; there is no mask for the moving image. {func}`~fastwarp.register_syn`
  takes no mask at all; only the [linear fits](linear.md#masks) do.
- **One stage per linear fit.** A {class}`~fastwarp.LinearSession` fit runs one rigid or one
  affine stage; chain several fits yourself.
- **No bias-field correction.** Run N4 (for example `ants.n4_bias_field_correction`) before
  FastWarp if your images need it.
- **Not bit-identical to ANTs.** The results agree closely with ANTs but differ in the last
  digits; see [Troubleshooting](../troubleshooting.md#results-differ-slightly-from-ants).

## Resampling

- **Three interpolators.** Linear, nearest neighbour and Lanczos windowed sinc. ANTs' `BSpline`,
  `Gaussian` and `GenericLabel` interpolators are not available.
- **Only linear transforms can be inverted.** `invert` (and `[file,1]` on the command line)
  inverts affine files and composites of affines. A displacement field cannot be inverted inside
  a transform list; use the inverse file FastWarp writes, such as `InverseComposite.h5` or
  `1InverseWarp.nii.gz`.
- **ANTsPy only in part.** [`fastwarp.ants`](ants.md) runs ANTsPy's registration and
  resampling calls within these limits and refuses the rest; ANTsPy's default
  `type_of_transform="SyN"` needs Mattes mutual information in SyN and is refused.
- **No point transforms.** To move points, use `antsApplyTransformsToPoints` or
  `ants.apply_transforms_to_points` with the [right file](outputs.md).
