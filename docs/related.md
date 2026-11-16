# Related projects

## Projects FastWarp builds on

[ANTs](https://github.com/ANTsX/ANTs)
: Advanced Normalization Tools, the image registration toolkit whose rigid, affine and
  {term}`SyN` registration FastWarp runs on the GPU. SyN was introduced by Avants et al.
  ([2008](https://doi.org/10.1016/j.media.2007.06.004)). FastWarp writes ANTs transform files,
  accepts the same schedule as `antsRegistration` (see
  [Recipes](guide/recipes.md#the-same-schedule-in-ants)), and its tests compare it with the
  programs of ANTs 2.6.5. When no GPU is available, ANTs is the fallback.

[ANTsPy](https://github.com/ANTsX/ANTsPy)
: The Python interface to ANTs (package `antspyx`). FastWarp accepts its `ANTsImage` objects
  directly, and {func}`~fastwarp.apply_transforms` takes the same transform lists as
  {func}`ants.apply_transforms`. ANTsPy is optional.

[ITK](https://itk.org)
: The Insight Toolkit, on which ANTs is built. FastWarp's transforms, metrics, optimizer and
  file formats are ports of ITK classes, including ITK's {term}`Mattes mutual information`
  metric (Mattes et al. [2003](https://doi.org/10.1109/TMI.2002.808367));
  [Third-party ports](THIRD_PARTY_PORTS.md) lists each ported file with its upstream source.

[wgpu](https://wgpu.rs)
: The Rust library through which FastWarp's compute shaders run on Vulkan, Metal and
  DirectX 12.

[TemplateFlow](https://www.templateflow.org)
: A public archive of brain templates. The [accuracy benchmark](benchmarks.md) and the figure
  on the home page use its MNI ICBM 2009c template.

## Projects that use FastWarp

[Wyss DBS Localizer](https://github.com/WyssCenter/dbs-localizer)
: A tool for localizing deep brain stimulation (DBS) electrodes in brain images. FastWarp was
  developed inside it as its registration engine, and its default recipe is the recipe of Wyss
  DBS Localizer ({func}`~fastwarp.cohort_recipe`).

## Other related projects

[Lead-DBS](https://www.lead-dbs.org)
: A toolbox for deep brain stimulation imaging, which can normalize images with ANTs. The
  earlier post-processing timings on the [Performance](performance.md) page are for the kind of
  steps it runs after a registration: composing the transforms into displacement fields and
  re-warping the subject's image. FastWarp does not depend on Lead-DBS.

## Similar tools

Other tools that register brain images quickly, and what FastWarp does differently. FastWarp's
aim is narrower than each of them: to run the ANTs recipe on GPUs of any vendor, through Vulkan,
Metal or DirectX 12, and write files that ANTs reads.

| Tool | What it is | How FastWarp differs |
| --- | --- | --- |
| [FireANTs](https://github.com/rohitrango/FireANTs) | A PyTorch library for diffeomorphic registration on GPUs, with its own Riemannian adaptive optimizer. | FastWarp ports the ANTs and ITK algorithms and writes ANTs files; it needs no PyTorch and no CUDA. |
| [NiftyReg](https://github.com/KCL-BMEIS/niftyreg) | Rigid, affine (block matching) and free-form deformation registration programs, with a CPU and a CUDA implementation. | FastWarp uses ANTs' metrics and SyN, and runs on Vulkan, Metal and DirectX 12 rather than CUDA. |
| [SynthMorph](https://martinos.org/malte/synthmorph/) | A deep-learning tool for affine and deformable registration, part of FreeSurfer as `mri_synthmorph`. | FastWarp optimizes each image pair, as ANTs does, instead of using a trained network. |
| [greedy](https://github.com/pyushkevich/greedy) | A fast CPU tool for deformable, affine and rigid registration that shares concepts with SyN but is not symmetric. | FastWarp runs symmetric SyN, on the GPU. |
| [elastix](https://github.com/SuperElastix/elastix) | A configurable registration toolbox based on ITK, with Python bindings (`itk-elastix`). | FastWarp offers one fixed set of methods, those of the ANTs recipe, with ANTs output files. |
