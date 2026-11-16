# User guide

FastWarp has three groups of functions:

- **Registration on the GPU.** {func}`~fastwarp.register_syn` runs a full normalization (Rigid,
  Affine and {term}`SyN`) and writes ANTs transform files. {func}`~fastwarp.linear_session`
  runs rigid or affine fits of one image pair.
- **Resampling on the CPU.** {func}`~fastwarp.apply_transforms` and
  {func}`~fastwarp.compose_field` apply ANTs transform files, like `antsApplyTransforms`.
- **GPU checks.** {func}`~fastwarp.gpu_available`, {func}`~fastwarp.gpu_info` and
  {func}`~fastwarp.fits_gpu` tell, before any work, whether a registration can run on this
  machine.

The same functions are available as the [command line](cli.md) `fastwarp`, which works on NIfTI
files. For scripts written for ANTsPy, [`fastwarp.ants`](guide/ants.md) takes the calls
`ants.registration` and `ants.apply_transforms` with their arguments and results.

The pages use the words of ANTs: the {term}`fixed image` is the reference (for example a
template), and the {term}`moving image` is the one that is aligned to it (for example a
subject's scan). The [glossary](glossary.md) explains the other terms.

::::{grid} 1 2 2 2
:gutter: 3

:::{grid-item-card} Images
:link: guide/images
:link-type: doc

What an image is: an `ANTsImage` or a NumPy array with its geometry.
:::

:::{grid-item-card} Normalization
:link: guide/normalization
:link-type: doc

`register_syn`: the stages, the registration grid, the result and cancelling a run.
:::

:::{grid-item-card} Output files
:link: guide/outputs
:link-type: doc

The transform files, which direction each one maps, and the `antsApplyTransforms` commands.
:::

:::{grid-item-card} Recipes
:link: guide/recipes
:link-type: doc

The registration schedule: every field, its type, unit, default and limits.
:::

:::{grid-item-card} Linear fits
:link: guide/linear
:link-type: doc

`linear_session`: rigid and affine fits, multi-start searches and masks.
:::

:::{grid-item-card} Resampling
:link: guide/resampling
:link-type: doc

`apply_transforms` and `compose_field` on the CPU.
:::

:::{grid-item-card} GPU and memory
:link: guide/gpu
:link-type: doc

Which GPU is used, the memory budget, what fits, and falling back to ANTs.
:::

:::{grid-item-card} ANTsPy compatibility
:link: guide/ants
:link-type: doc

`fastwarp.ants.registration` and `apply_transforms`: ANTsPy's calls, run on the GPU.
:::

:::{grid-item-card} Command line
:link: cli
:link-type: doc

The `fastwarp` command for NIfTI files: register, fit, apply, compose, benchmark.
:::

:::{grid-item-card} What FastWarp does not do
:link: guide/limitations
:link-type: doc

The limits of the package, in one place.
:::
::::

## How it works

FastWarp ports the registration code of ANTs and [ITK](https://itk.org), the toolkit ANTs is
built on, to GPU compute shaders that run through [wgpu](https://wgpu.rs). It follows the
methods of these papers and programs:

- **Rigid and affine stages:** {term}`Mattes mutual information` (Mattes et al. 2003) with
  regularly spaced samples, optimized by a conjugate-gradient method with a line search, as
  `antsRegistration` does.
- **Deformable stage:** {term}`SyN` (Avants et al. 2008), with {term}`cross-correlation` in a
  small window, Gaussian smoothing of the deformation, and an inverse computed at each step.
- **Files and conventions:** ITK's transform file formats, its {term}`LPS` coordinates, and the
  ANTs way of naming and chaining transform files.

The engine works on the GPU in 32-bit floating point, where ANTs uses 64-bit numbers on the CPU,
so results agree closely with ANTs but are not identical (see
[Troubleshooting](troubleshooting.md#results-differ-slightly-from-ants)). Before a
normalization starts, FastWarp checks whether a GPU is present and has enough memory, so that a
pipeline can fall back to ANTs when it does not. [Third-party ports](THIRD_PARTY_PORTS.md) lists
every ported ITK and ANTs file.

References:

- Mattes D, Haynor DR, Vesselle H, Lewellen TK, Eubank W (2003). PET-CT image registration in
  the chest using free-form deformations. *IEEE Transactions on Medical Imaging* 22(1):120–128.
  [doi:10.1109/TMI.2002.808367](https://doi.org/10.1109/TMI.2002.808367)
- Avants BB, Epstein CL, Grossman M, Gee JC (2008). Symmetric diffeomorphic image registration
  with cross-correlation: evaluating automated labeling of elderly and neurodegenerative brain.
  *Medical Image Analysis* 12(1):26–41.
  [doi:10.1016/j.media.2007.06.004](https://doi.org/10.1016/j.media.2007.06.004)

```{toctree}
:hidden:

guide/images
guide/normalization
guide/outputs
guide/recipes
guide/linear
guide/resampling
guide/gpu
guide/ants
cli
guide/limitations
```
