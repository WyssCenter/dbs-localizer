# FastWarp

[![CI](https://github.com/WyssCenter/fastwarp/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/WyssCenter/fastwarp/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/fastwarp)](https://pypi.org/project/fastwarp/)
[![Python versions](https://img.shields.io/pypi/pyversions/fastwarp)](https://pypi.org/project/fastwarp/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/WyssCenter/fastwarp/blob/main/LICENSE)
[![Documentation](https://readthedocs.org/projects/fastwarp/badge/?version=latest)](https://fastwarp.readthedocs.io)

FastWarp registers 3-D images on the GPU. It runs the rigid, affine and SyN registration of
[ANTs](https://github.com/ANTsX/ANTs) on a graphics card and writes the same transform files as
ANTs, so the rest of an ANTs-based pipeline can stay as it is. It is meant for people who build
or run neuroimaging pipelines, for example to normalize brain MRI onto a template.

- A full normalization (centre of mass, Rigid, Affine, SyN) in one call, with one or two
  channels such as T1w + T2w.
- Rigid and affine fits of one image pair, several in a row on images prepared once.
- Resampling and field composition like `antsApplyTransforms`, on the CPU.
- A command line, `fastwarp`, for the same steps on NIfTI files.
- Runs on Vulkan (Linux, Windows), Metal (macOS) and DirectX 12 (Windows) through
  [wgpu](https://wgpu.rs). Before a normalization starts, it checks that a GPU is present and
  has enough memory, so a pipeline can fall back to ANTs.

**Documentation: <https://fastwarp.readthedocs.io>**

## Install

```bash
pip install fastwarp
uv add fastwarp
```

The first line is for pip, the second for a uv project (`uv pip install fastwarp` installs into
an existing environment). Wheels are published on PyPI and attached to each
[GitHub release](https://github.com/WyssCenter/fastwarp/releases) for Linux (x86_64, aarch64),
macOS (Apple silicon, Intel) and Windows (x86_64), for Python 3.11 and later. FastWarp needs
NumPy 2.0 or later and a GPU; [ANTsPy](https://github.com/ANTsX/ANTsPy) (`pip install antspyx` or
`uv add antspyx`; wheels for Python 3.11 to 3.13 on Linux x86_64, macOS and Windows) is optional.

The command line `fastwarp` comes with the Python package. `uv tool install fastwarp` installs
only the command and `uvx fastwarp --help` runs it without installing. From release 0.2.0 it
also comes as a standalone program without Python, with installer scripts for Linux, macOS and
Windows ([Command line: install](https://fastwarp.readthedocs.io/latest/cli.html#install)).
The command line needs release 0.2.0 or later;
`uvx --from "git+https://github.com/WyssCenter/fastwarp.git" fastwarp --help` runs the
development version from source.
[Installation](https://fastwarp.readthedocs.io/latest/installation.html) has the requirements
and the build from source.

## Example

Register ANTsPy's `ch2` sample brain onto its MNI template, then warp it with ANTsPy:

```python
import ants
import fastwarp

fixed = ants.image_read(ants.get_ants_data("mni"))
moving = ants.image_read(ants.get_ants_data("ch2"))

if fastwarp.fits_gpu(fixed, moving, iso_mm=1.0):
    info = fastwarp.register_syn(fixed, moving, "out/ch2_to_mni_")
    warped = ants.apply_transforms(fixed, moving, ["out/ch2_to_mni_Composite.h5"])
    print(info["adapter"], f"{info['total_s']:.1f} s")
else:
    print("No GPU, or too little GPU memory: use ANTs instead.")
```

`register_syn` writes `out/ch2_to_mni_Composite.h5`, `InverseComposite.h5` and
`0GenericAffine.mat`, which `antsApplyTransforms`, `antsApplyTransformsToPoints` and ANTsPy read
as they read ANTs' own files.
[Output files](https://fastwarp.readthedocs.io/latest/guide/outputs.html) says which file maps
points or images in which direction.

## Command line

The package also installs the command `fastwarp` (the same as `python -m fastwarp`), which works
on NIfTI files and runs the same code as the Python functions:

```bash
fastwarp register tpl.nii.gz sub.nii.gz -o out/sub_
fastwarp apply -r tpl.nii.gz -i sub.nii.gz -o out/sub_in_tpl.nii.gz -t out/sub_Composite.h5
fastwarp bench
```

`fastwarp bench` times FastWarp on built-in synthetic volumes. Unlike `register_syn`, which
registers on a 1.0 mm grid unless told otherwise, `fastwarp register` uses the smallest voxel
spacing of the fixed image; `--iso-mm` sets it. The exit status tells a script what happened, for
example 4 for no usable GPU and 5 for a grid too large for the GPU. Reference:
[Command line](https://fastwarp.readthedocs.io/latest/cli.html).

## Learn more

- [Quickstart](https://fastwarp.readthedocs.io/latest/quickstart.html): first registration, resampling and a rigid fit.
- [User guide](https://fastwarp.readthedocs.io/latest/user-guide.html): images, output files, recipes, linear fits and GPU memory.
- [API reference](https://fastwarp.readthedocs.io/latest/api.html) and [environment variables](https://fastwarp.readthedocs.io/latest/environment.html).
- [Performance](https://fastwarp.readthedocs.io/latest/performance.html) and the [accuracy benchmark](https://fastwarp.readthedocs.io/latest/benchmarks.html): how speed and accuracy are measured against ANTs on public data. No benchmark results yet; earlier measurements on non-public data are on the Performance page, labelled as such.
- [Troubleshooting](https://fastwarp.readthedocs.io/latest/troubleshooting.html): no GPU found, too little GPU memory.

## Project

FastWarp was developed as the registration engine of
[Wyss DBS Localizer](https://github.com/WyssCenter/dbs-localizer) and is maintained by Richard
Köhler (Wyss Center for Bio and Neuroengineering) and Jan Philipp Payonk (University of
Rostock). It is research software;
support is given on a best-effort basis through
[GitHub issues](https://github.com/WyssCenter/fastwarp/issues). Contributions are welcome: see
[CONTRIBUTING.md](https://github.com/WyssCenter/fastwarp/blob/main/CONTRIBUTING.md).

If you use FastWarp, please cite it as described in
[CITATION.cff](https://github.com/WyssCenter/fastwarp/blob/main/CITATION.cff) and
[Citing FastWarp](https://fastwarp.readthedocs.io/latest/citing.html).

Licence: [Apache-2.0](https://github.com/WyssCenter/fastwarp/blob/main/LICENSE). Parts of FastWarp
are ports of ANTs and [ITK](https://itk.org), which are Apache-2.0 as well; their notices are in
[NOTICE](https://github.com/WyssCenter/fastwarp/blob/main/NOTICE) and each ported file is listed
in [Third-party ports](https://fastwarp.readthedocs.io/latest/THIRD_PARTY_PORTS.html).
