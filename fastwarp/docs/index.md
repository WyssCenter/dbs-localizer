# FastWarp

FastWarp is a Python package for GPU image registration. It runs a full Rigid, Affine and SyN
normalization of 3-D images in one call, and rigid or affine fits (coregistration, template
alignment, brain-shift refinement) in a session that keeps the image pair on the GPU. It was
developed by Richard Köhler (Wyss Center for Bio and Neuroengineering) and Jan Philipp Payonk
(University of Rostock) as the registration engine of Wyss DBS Localizer, and is packaged as a
wheel.

The engine ports the registration of ANTs/ITK to GPU compute shaders through
[wgpu](https://wgpu.rs). It supports the Mattes mutual information and cross-correlation
metrics, conjugate-gradient linear stages, and SyN with Gaussian field regularisation and field
inversion. It runs on Metal (macOS), Vulkan (Linux, Windows) and DX12 (Windows). The outputs
are ANTs files (`Composite.h5`, `InverseComposite.h5`, `0GenericAffine.mat`) that ANTsPy and
`antsApplyTransforms` read as is.

## Statement of need

A SyN normalization of a T1w and T2w pair onto a 0.5 mm template takes ANTs 1000 to 1500 s on 16
CPU threads. On a desktop or laptop GPU the same registration takes 12 to 24 s in the
[measurements](performance.md). A pipeline that registers many subjects, or runs several linear
fits per subject, gains from that difference.

FastWarp keeps the ANTs recipe, file formats and conventions, so that a pipeline replaces the
`antsRegistration` call and keeps the rest. It reports a GPU that is missing or too small before
any work starts (`GpuUnavailableError`, `GpuCapacityError`), so that the caller can fall back to
ANTs.

## Who it is for

Developers of neuroimaging pipelines and researchers who register many 3-D images with ANTs and
want the same transforms faster. The package does not read or write NIfTI images for you: images
come from ANTsPy (`ANTsImage`) or are passed as arrays with their geometry.

TODO(author): name the intended audience beyond the Wyss DBS Localizer and Lead-DBS style
workflows, if any.

Source code: <https://github.com/WyssCenter/fastwarp>. Please cite the engine as in
`CITATION.cff`; the text is also available as `fastwarp.CITATION`.

```{toctree}
:hidden:

installation
quickstart
user-guide
api/index
testing
performance
changelog
licence
THIRD_PARTY_PORTS
```
