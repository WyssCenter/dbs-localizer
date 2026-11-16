# Glossary

The words this documentation uses, in the sense of [ANTs](https://github.com/ANTsX/ANTs) and
[ITK](https://itk.org).

```{glossary}
Adapter
  A GPU as the graphics driver presents it, for example "NVIDIA RTX A4000". A machine can have
  several. See [Which GPU is used](guide/gpu.md#which-gpu-is-used).

Affine
  A linear transform with 12 parameters: rotation, translation, scaling and shearing. ITK
  stores it as a 3 × 3 matrix, a translation and a centre of rotation.

ANTsImage
  The image class of [ANTsPy](https://github.com/ANTsX/ANTsPy): voxel data with its spacing,
  origin and direction. FastWarp accepts it directly. See [Images](guide/images.md).

Capacity check
  The test, before a {term}`SyN` registration starts, of whether the {term}`registration grid`
  fits the GPU's memory budget and the largest buffer the device allows. See [What fits](guide/gpu.md#what-fits).

Centre of mass
  The intensity-weighted mean position of an image. Translating the moving image's centre of
  mass onto the fixed image's is the usual start of a registration, as
  `antsRegistration --initial-moving-transform [fixed,moving,1]` does.

Channel
  One image of a set that is registered together, for example a T1-weighted and a T2-weighted
  scan of the same subject. FastWarp registers one or two channels.

Coregistration
  Aligning two images of the same subject, for example a CT onto an MRI scan, usually with a
  {term}`rigid` transform.

Composite transform
  One file that holds a chain of transforms applied in order. FastWarp writes
  `Composite.h5`, an {term}`affine` followed by a {term}`displacement field`, and its inverse.

Cross-correlation
  The similarity measure of the {term}`SyN` stage: the correlation of the two images'
  intensities in a small window around each voxel. It works when both images have the same
  contrast. ANTs calls it `CC`.

Displacement field
  A transform given as one vector per voxel: the shift, in millimetres, of each point.

Fixed image
  The image that another one is aligned to, for example a template. Its space is the output
  space of a registration.

Graphics API
  The interface between a program and the GPU driver: Vulkan, Metal or DirectX 12. wgpu, which
  FastWarp uses, also calls it a backend.

LPS
  The coordinate convention of ITK and ANTs: x grows towards the patient's left, y towards
  posterior, z towards superior. NIfTI files and nibabel use RAS, in which x and y have the
  opposite sign.

Mattes mutual information
  The similarity measure of the rigid and affine stages. It measures how well the intensities
  of one image predict those of the other, so it also works between different contrasts, such
  as CT and MRI. ANTs calls it `MI`.

Moving image
  The image that is aligned to the {term}`fixed image`, for example a subject's scan.

Recipe
  The schedule of a registration: the start, the stages, and the iterations, resolution levels,
  smoothing and metric settings of each stage. See [Recipes](guide/recipes.md).

Stage list
  The form of a {term}`recipe` that names the start and the stages one by one, for example only a
  rigid and an affine stage. See [Stage lists](guide/recipes.md#stage-lists).

Registration grid
  The isotropic grid, with `iso_mm` spacing, on which {func}`~fastwarp.register_syn` registers.
  See [The registration grid](guide/normalization.md#the-registration-grid).

Rigid
  A transform with six parameters: three rotations and three translations. It keeps distances
  and angles, so it suits two images of the same head.

SyN
  Symmetric normalization, the deformable registration of ANTs (Avants et al. 2008). It
  deforms both images towards a midpoint and keeps the deformation smooth and invertible, so
  that every registration also gives its inverse.

T1w, T2w
  T1-weighted and T2-weighted MRI scans, the two most common contrasts of structural brain
  MRI.

Template
  An average brain image in a standard space, such as the MNI 2009 templates. Registering
  images onto a template puts them in one common space.

Winsorize
  Clip the darkest and brightest voxels to fixed quantiles of the intensity distribution (in
  {func}`~fastwarp.register_syn` always the 0.005 and 0.995 quantiles; in
  {func}`~fastwarp.linear_session` only when asked), then rescale the intensities to [0, 1], so
  that a few extreme voxels do not dominate the metric.
```
