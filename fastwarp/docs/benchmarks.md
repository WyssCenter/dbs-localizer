# Accuracy benchmark

This page describes a benchmark that measures how accurately FastWarp registers brain MRI, on
public data with expert ground truth, how closely it agrees with ANTs running the same recipe,
and how long both take. Reviewers can download the data and run it themselves. The code is in
`benchmarks/accuracy/` of the repository; it is not part of the wheel. The timings it records
are the ones [Performance](performance.md) reports.

No results are reported here yet. The benchmark has been tested end to end only on a
synthetic phantom (`tests/test_benchmark_smoke.py`); the real-data runs are still to be done,
and their tables will be added to this page when they are.

## What is measured

Each registration pair is registered twice, once by FastWarp on the GPU and once by
`antsRegistration` on the CPU, with the same recipe. For each pair and engine:

- against ground truth: Dice and Jaccard overlap per labelled structure, the 95th-percentile
  Hausdorff distance (HD95) and the average symmetric surface distance (ASSD), each averaged
  per structure group (cortical, subcortical, deep nuclei); and the landmark error (mm) per
  anatomical fiducial;
- regularity: the fraction of voxels with a Jacobian determinant ≤ 0 (folding) and the
  minimum, 1st percentile, median and maximum of the determinant, inside the fixed image's
  brain mask;
- against each other: the RMS, mean, 95th percentile and maximum of the difference between
  the two engines' forward displacement fields inside the brain mask, and the Dice overlap
  between the label maps the two engines warp;
- time: see [Timings](#timings).

For every per-pair metric the report gives both engines' mean and SD, the mean paired
difference FastWarp minus ANTs with a 95 % percentile bootstrap confidence interval (10 000
resamples, fixed seed), and the two-sided Wilcoxon signed-rank p-value.

## Datasets

Every file is downloaded from the address below and checked against a SHA-256 pinned in
`benchmarks/accuracy/pins.json` before it is used. None of the data is stored in the
repository. Licences were read on the primary sources on 2026-11-12; quotes are verbatim.

| Key | Ground truth | Images | Subjects | Download | Default |
| --- | --- | --- | --- | --- | --- |
| `mindboggle` | manual cortical (DKT31) and subcortical labels | 1.5 T T1w, skull-stripped, 1 mm | 20 | 0.42 GB | yes |
| `deep7t` | manual STN, SN, RN, GPe, GPi masks | 7 T MP2RAGE T1w, skull-stripped, 0.7 × 0.64 × 0.64 mm | 10 | 2.85 GB | yes |
| `snsx` | 32 anatomical fiducials (AFIDs) | 7 T MP2RAGE T1w, 0.7 mm | 32 | 1.09 GB | yes |
| `lhscpd` | 32 anatomical fiducials (AFIDs) | 1.5 T contrast-enhanced T1w | 40 | 0.34 GB | optional |

Open a dataset below for its source, images, ground truth, licence and citation.

:::{dropdown} Mindboggle-101, OASIS-TRT-20 (cortical and subcortical labels)
- Source: the Mindboggle-101 project on OSF, <https://osf.io/nhtur/>. Images and cortical
  labels: `OASIS-TRT-20_volumes.tar.gz` (<https://osf.io/download/tyvhs/>, 408 MB; only
  `t1weighted_brain.nii.gz` of each subject is extracted). Whole-brain labels:
  `OASIS-TRT-20_DKT31_CMA_labels_v2/OASIS-TRT-20-<n>_DKT31_CMA_labels.nii.gz` (20 files of
  about 350 kB, <https://osf.io/zevma/>).
- Images: 20 healthy adults of OASIS-1 (Marcus et al. 2007), 1.5 T MP-RAGE, distributed by
  Mindboggle on a 256 × 256 × 160 grid of 1 mm voxels, skull-stripped. The labels are on the
  same grid (checked on the headers of every subject's brain image and of subject 1's labels).
- Ground truth: the DKT31 cortical labels (31 regions per hemisphere) and 16 subcortical
  structures (thalamus, caudate, putamen, pallidum, hippocampus, amygdala, accumbens, ventral
  diencephalon; FreeSurfer numbering). The cortical labels started from FreeSurfer's automatic
  labelling and were then edited by hand (Klein and Tourville 2012: "FreeSurfer's DK classifier
  atlas assigned the initial labels … These were then manually edited … to conform to the DKT
  protocol"). The subcortical labels are described by the data provider as manual ("The
  manually labeled subcortical portions of the 'WholeBrain' OASIS-TRT-20 labels"); the
  labelling protocol of these files is not documented next to them. The archive's
  `labels.DKT31.manual+aseg.nii.gz` files, whose subcortical part is FreeSurfer's automatic
  `aseg`, are not used.
- Cite: Klein A, Tourville J (2012), *Front Neurosci* 6:171, doi:10.3389/fnins.2012.00171;
  Marcus DS et al. (2007), *J Cogn Neurosci* 19(9):1498–1507, doi:10.1162/jocn.2007.19.9.1498.

- Licence: The project README: "All labeled data, including nifti volumes (nii), vtk surfaces
  (vtk), and FreeSurfer files (mgh, etc.) for each scanned "GROUP" … are licensed under a
  by-attribution Creative Commons License" (<http://creativecommons.org/licenses/by/4.0>), and
  "The manually labeled subcortical portions of the "WholeBrain" OASIS-TRT-20 labels are
  licensed under a similar Creative Commons License" (<http://creativecommons.org/licenses/by-nc-nd/4.0>).
  The `README_subcortical_labels.txt` beside the label files says the same: "released under a
  different Creative Commons License (http://creativecommons.org/licenses/by-nc-sd/4.0)" (sic).
  The OSF component itself shows CC BY 4.0. We follow the stricter statement: the subcortical
  labels are for non-commercial use without derivatives being distributed. The images derive
  from OASIS-1, whose own terms ("OASIS data are provided for non-commercial, academic research
  purposes only", <https://sites.wustl.edu/oasisbrains/>) ask for the acknowledgement "Data were
  provided [in part] by OASIS"; whether they still bind Mindboggle's CC BY redistribution is not
  stated, so treat the images as non-commercial research data as well.
:::

:::{dropdown} 7 T deep nuclei (STN, SN, RN, GPe, GPi)
- Source: the data of Isaacs et al. (2020) on OSF, <https://osf.io/4nrku/>, one archive
  `dataset/osf_dataset.tar.gz` (<https://osf.io/download/59kjv/>, 2.85 GB). Only the 7 T part is
  used: per subject the skull-stripped MP2RAGE image (`<id>_7T_WB_MP2RAGE_STD_BET.nii.gz`), its
  brain mask (`<id>_7T_WB_INV2_e1_M_STD_BET_mask.nii.gz`) and ten structure masks
  (`Masks/<structure>/calc/<id>_7T_<structure>_<L|R>_AA-BI_conj.nii.gz`); 122 MB once extracted.
- Images: 10 healthy young adults, 7 T MP2RAGEME, the T1-weighted MP2RAGE image on a
  234 × 320 × 320 grid of 0.7 × 0.64 × 0.64 mm. The masks and the brain mask share that grid
  exactly (checked on the headers of every subject): MP2RAGEME acquires the T1-weighted image and
  the multi-echo images the structures were delineated on in one sequence, so no registration
  between contrasts is involved.
- Ground truth: subthalamic nucleus, substantia nigra, red nucleus, external and internal
  globus pallidus, left and right, delineated by hand by two raters (AA and BI); the `AA-BI_conj`
  files are the voxels both raters included (the conjunction masks of the article).
- Not used: the 3 T part of the archive. Its masks lie on the grid of a separate T2\*-weighted
  scan, so a T1w registration would add the motion between two scans to the error.
- Cite: Isaacs BR et al. (2020), *PLoS One* 15:e0236208, doi:10.1371/journal.pone.0236208.

- Licence: The article's data availability statement: "All anonymized data and analysis scripts
  are available from https://osf.io/4nrku/, under the terms of the Creative Commons Attribution
  License and complies with the rules of the General Data Protection Regulation (EU) 2016/679."
  The OSF project itself shows no licence.
:::

:::{dropdown} SNSX: AFIDs on 7 T images (landmarks)
- Source: OpenNeuro ds004470 version 1.0.1, <https://openneuro.org/datasets/ds004470/versions/1.0.1>;
  the files are fetched through OpenNeuro's version 1.0.1 snapshot links.
- Images: 32 healthy adults (20–70 years), 7 T MP2RAGE T1w, 224 × 342 × 342 voxels of
  0.7 × 0.70 × 0.70 mm, background set to zero by the provider.
- Ground truth: the 32 anatomical fiducials of the AFIDs protocol (Lau et al. 2019) per subject,
  `derivatives/afids_groundtruth` ("Placed each point within 3D Slicer, in subject space"), the
  mean of three placements according to the dataset README ("performed a total of 3 times on all
  subject scans"). Taha et al. (2023) report rater accuracy within 1–2 mm.
- Template: MNI ICBM 2009c nonlinear asymmetric (`MNI152NLin2009cAsym`) at 1 mm from
  TemplateFlow
  ([`tpl-MNI152NLin2009cAsym_res-01_T1w.nii.gz`](https://templateflow.s3.amazonaws.com/tpl-MNI152NLin2009cAsym/tpl-MNI152NLin2009cAsym_res-01_T1w.nii.gz)
  and
  [`..._desc-brain_mask.nii.gz`](https://templateflow.s3.amazonaws.com/tpl-MNI152NLin2009cAsym/tpl-MNI152NLin2009cAsym_res-01_desc-brain_mask.nii.gz)),
  with the AFIDs placed on it
  (`tpl-MNI152NLin2009cAsym_res-01_desc-groundtruth_afids.fcsv` of
  <https://github.com/afids/tpl-MNI152NLin2009cAsym>, commit `a44e5a7`). The template image's
  MD5 equals the git-annex key of the image the AFIDs were placed on in that repository. Template
  licence (its `LICENSE`): "Permission to use, copy, modify, and distribute this software and its
  documentation for any purpose and without fee is hereby granted, provided that the above
  copyright notice appear in all copies." AFIDs annotations: "All anatomical landmark coordinate
  data released here (i.e., *.fcsv extension) are under a CC-BY-4.0 license"
  (<https://github.com/afids/afids-data>).
- Cite: Lau JC et al. (2020), *Hum Brain Mapp* 41:4500–4517; Taha A et al. (2023), *Sci Data*
  10:449, doi:10.1038/s41597-023-02330-9; Lau JC et al. (2019), *Hum Brain Mapp* 40:4163–4179,
  doi:10.1002/hbm.24695; Fonov V et al. (2011), *NeuroImage* 54:313–327,
  doi:10.1016/j.neuroimage.2010.07.033.

- Licence: `LICENSE` of the dataset is CC0 1.0 Universal, and `dataset_description.json` says
  `"License": "CC0"`. No data-use agreement.
:::

:::{dropdown} LHSCPD: AFIDs on 1.5 T images (optional)
OpenNeuro ds004471 version 1.0.1 (CC0, same AFIDs layout as SNSX): 40 patients with Parkinson's
disease before deep brain stimulation surgery, gadolinium-enhanced 1.5 T T1w. It is not in the
default run, because contrast enhancement makes the images unlike the template; run it with
`--dataset lhscpd`. Cite Abbass M et al. (2022), *Brain Struct Funct* 227:393–405, and Taha
et al. (2023).
:::

:::{dropdown} Considered and not used
| Dataset | Why not (or how to add it) |
| --- | --- |
| LPBA40 (40 subjects, 56 manually labelled structures; Shattuck et al. 2008) | Download from LONI after registering and accepting the LONI Research License; no anonymous download to pin. |
| IBSR18 (18 subjects, manual subcortical and cortical labels) | NITRC requires a log-in for the download ("The tool/resource administrator has requested that you log in to download this file", checked 2026-11-12); licence "Free for non-commercial use only". |
| CUMC12, MGH10 (Klein et al. 2009) | No current primary download location found. |
| AFIDs-OASIS, AFIDs-HCP | The fiducials are CC BY 4.0, but the images need the OASIS or HCP data-use agreement. |
| Hammers adult atlases (30 subjects, 95 regions including substantia nigra) | Registration and licence agreement at brain-development.org. |
| CIT168, ATAG, MNI PD25 | Templates with probabilistic or template-space labels; no individual subjects with their own manual labels. |
| Mindboggle `labels.DKT31.manual+aseg` subcortical part | FreeSurfer's automatic `aseg`, not manual. |
| The 3 T part of the Isaacs et al. (2020) archive | Masks on the grid of a separate T2\*-weighted scan, not of the T1w. |
| AHEAD (Alkemade et al. 2020), MASSP training data (Bazin et al. 2020) | Not checked in depth; the 7 T set above covers the same structures with an anonymous download. |
:::

## Protocol

### Pairs

- `mindboggle`: inter-subject pairs, as in Klein et al. (2009), who evaluated 14 nonlinear
  methods on all ordered pairs of labelled brains. All 380 ordered pairs of 20 subjects do not fit
  a night with ANTs, so the subjects (in natural order) form rings: subject *i* is registered onto
  subject *i + k* for *k* = 1, 2, … in turn. The default, *k* = 1, gives 20 pairs in which every
  subject is fixed once and moving once.
- `deep7t`: the same rings over its 10 subjects; the default is 10 pairs.
- `snsx` and `lhscpd`: each subject is registered onto the MNI 2009c template, as a normalisation
  pipeline does. The template's fiducials are moved into the subject through the forward transform
  and compared with the subject's own (in mm, in subject space). The default takes the first 20
  subjects in natural order; `--pairs 32` takes all of SNSX.

### Images given to the engines

- The fixed image is cropped to its non-zero voxels plus 10 voxels (skull-stripped datasets only),
  resampled linearly to a 1 mm isotropic grid when its spacing is not 1 mm, and written once.
  FastWarp registers on an isotropic grid built from the fixed image; giving both engines this one
  file makes them register on the same grid.
- The moving image is resampled the same way (linear, no crop) when its spacing is not 1 mm, and
  both engines get that file. FastWarp would otherwise resample the moving image to the
  registration grid itself, while ANTs samples the native image, and the two would see different
  inputs (this matters for the 0.7 mm 7 T images). The labels, masks and landmarks used for the
  evaluation stay the original files; only physical space matters for them.
- T1w only (one channel): none of the default datasets has a T2w aligned to the T1w.
- No bias-field correction, no histogram matching, winsorising to the [0.005, 0.995] quantiles
  in both engines.

### Recipe

`--recipe cohort` (default) is FastWarp's default recipe (`fastwarp.cohort_recipe(1)`), the one
the benchmark timings in [Performance](performance.md) use. ANTs gets the identical schedule:

```{literalinclude} _includes/ants-cohort-command.txt
:language: text
```

`--recipe syn-sh` is what `antsRegistrationSyN.sh -t s` of ANTs 2.6.5 runs on an image below its
"large image" size: the same, except `SyN[0.2,3,0]` and `CC` radius 2 (the script's defaults
`SYNGRADIENTSTEP=0.2` and `CCRADIUS=2`, lines 23 and 26 of `Scripts/antsRegistrationSyN.sh` at the
ANTs tag `v2.6.5`; older versions of the script used 0.1 and 4). Both engines run it the same way. The test
`tests/test_benchmark_recipes.py` pins the command above and checks that `cohort` equals
`fastwarp.cohort_recipe(1)`. `--verbose 1` only makes ANTs print its progress and stage times,
which the benchmark reads (see [Timings](#timings)); it does not change the registration.

ANTs runs as a child process, one at a time, never at the same time as FastWarp, on all cores
unless `--threads` caps it (see [Timings](#timings)). By default it is ANTsPy's
bundled `antsRegistration`; `--ants-dir` runs an `antsRegistration` program instead (for example
the ANTs 2.6.5 release the CI uses). Precision is ANTs' default (double). A fixed seed does not
make ANTs repeatable with more than one thread, because its threads sum in any order; FastWarp
runs are deterministic on one GPU.

### Evaluation

Both engines' outputs go through the same ANTsPy calls, so differences come from the
transforms, not from the resampling code:

- the moving labels (or each moving structure mask) are warped onto the fixed evaluation grid
  (the cropped native grid of the fixed labels) with `nearestNeighbor`, as in Klein et al. (2009);
- the forward composite is composed into one displacement field on that grid
  (`antsApplyTransforms --compose`); a fixed landmark *p* moves to *p* + *u*(*p*) with trilinear
  interpolation of the field, as `antsApplyTransformsToPoints` moves points through a
  displacement field; landmarks are read from the Slicer `.fcsv` files (RAS) and converted to
  ITK's LPS;
- the Jacobian determinant of *x* ↦ *x* + *u*(*x*) is computed by central differences in
  physical coordinates;
- surface distances use the 6-connected boundary voxels of each mask and the Euclidean distance
  transform with the voxel spacing; HD95 is the larger of the two directed 95th percentiles,
  ASSD the mean over both directions;
- only structures present in the fixed reference are scored, so both engines are scored on the
  same structures; a structure the warp loses entirely gets Dice 0 and no distance, and is counted
  in `missing_<group>` of `pairs.csv`;
- structures are grouped (`cortical`: DKT31; `subcortical`: the 16 Mindboggle structures;
  `deep_nuclei`: the ten 7 T masks); a group's value per pair is the mean over its structures
  (HD95 and ASSD over the structures that have a distance). The 7 T structure masks are warped
  and scored one by one, so overlapping masks are kept as they are;
- a landmark pair whose two files do not share all their labels is refused, so a mean error is
  never taken over a silent subset.

The pairs of a ring share subjects, so they are not independent; the bootstrap intervals and
p-values treat them as if they were, and should be read as descriptive.

### Timings

Three steps record timings; each writes them next to its outputs, with the machine and software
(`environment` in every record), and `report` tabulates the median and quartiles over pairs,
per dataset and registration grid, from the records of the recipe as it is defined now. A ratio
ANTs / FastWarp is the median over pairs of the per-pair ratio, on the pairs where both engines
have the value and ran on the same machine and software (GPU, driver name, backend, CPU, thread
setting, fastwarp and ANTs versions); a cell marked `[n=k]` has a value for only k pairs.

**Threads.** By default the timings are taken with all cores: `run`, `linear` and `post` set
no thread limit for either engine, and remove every variable that sets one from the
environment they and ANTs run in: for ITK (ANTs, and ANTsPy in this process)
`ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS`, `NSLOTS` and `ITK_NUMBER_OF_THREADS_ENV_LIST` with the
names it lists, which is what ITK 5 reads; for FastWarp `RAYON_NUM_THREADS` and
`RAYON_RS_NUM_CPUS`. The values the caller had set are recorded (`thread_env_of_caller`).
`--threads N` caps both engines alike,
for when the machine is needed during a run: `ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS=N` in the
environment of every ANTs process (and, restored afterwards, around this process's own ANTsPy
calls such as reading and resampling the images), and `RAYON_NUM_THREADS=N` for FastWarp's CPU
work (resampling, preprocessing, composing fields, resampling images), set before FastWarp is
first used; FastWarp reads it once, when its thread pool starts (opening the GPU starts it). The
GPU work itself is not affected. Every record's `environment` states the setting (`threads`:
`"all cores"` or N) and the logical and physical core counts (the physical count is read on
Linux and macOS only), and the report lists it per command. Timings taken with different
settings are not comparable: the report keeps them in separate rows (column Threads).

**Registration (`run`).** For every registration, both engines:

| Field | FastWarp | ANTs |
| --- | --- | --- |
| `linear_s` | the engine's `linear_s`: centre of mass, rigid and affine stages | the sum of `Elapsed time (stage 0)` and `(stage 1)` (rigid, affine) |
| `syn_s` | the engine's `syn_s`: the SyN stage with its smoothing and transfers | `Elapsed time (stage 2)` (SyN) |
| `total_s` | the engine's `total_s`: opening the GPU, resampling, preprocessing, both stages, warping the moving image onto the registration grid (`warp_s`) and writing the transforms | `Total elapsed time`: the stages and their set-up, without reading the images or writing the transforms |
| `wall_s` | the `register_syn` call, with the images already read (`read_s` records reading them) | the `antsRegistration` process, including its start, reading and writing (with ANTsPy also starting Python) |

ANTs prints these lines with `--verbose 1` (ITK's wall-clock `TimeProbe`, in seconds); the
stage a time belongs to is the one named in the `*** Running <transform> registration ***` line
before it (`Euler3DTransform`, `AffineTransform` and `SyN` for the benchmark's recipes; any
other name leaves the split empty). When a log lacks a line (an interrupted run, or a run
recorded before the benchmark passed `--verbose 1`), the missing part is left empty and
`run.json` says why (`timings.note`); nothing is estimated. Because the two totals and the two
wall times do not cover the same work, compare them as what they are: the engines' own
accounting, and the time a caller waits.

**Linear fits (`linear`).** On the same prepared images as `run` (the fixed and moving images of
a pair, at the registration grid), with the recipe's `rigid` and `affine` stages:

- FastWarp: one `fastwarp.linear_session` per pair; its preparation (`prepare_s`: opening the
  GPU, converting, winsorizing and uploading the images, the centre-of-mass start) is timed on
  its own. Then a rigid fit from the centres of mass, an affine fit from that rigid result, and
  a rigid multi-start: a coarse rigid fit (the first two levels) from the centre-of-mass start
  rotated by 0°, ±15° about x and ±15° about z, then the two best (lowest MI) refined with the
  full rigid stage. A fit's time is the `total_s` that `LinearSession.register` returns.
- ANTs: `antsRegistration` with only the rigid stage (centre-of-mass start), then a second
  process with only the affine stage starting from the rigid result (`--initial-moving-transform
  [rigid_0GenericAffine.mat]`). A fit's time is that stage's `Elapsed time`, which includes the
  stage's own image preparation (winsorizing, smoothing); the process wall time is kept as well.
  ANTs has no multi-start option, so the multi-start has no ANTs time.
- Like for like: the report's "rigid + affine" row adds FastWarp's preparation to its two fits
  and compares them with ANTs' two stage times.
- Accuracy (`comparison.json`, made from the records of the same configuration and grid): the
  mean landmark error of each fit (the fixed image's landmarks moved through the affine, against
  the moving image's; datasets with landmarks only), and the mean and maximum distance between
  where FastWarp's and ANTs' fits move the points of the fixed brain mask (every second voxel),
  the multi-start against ANTs' rigid fit. A comparison that fails is a failed row, and the
  command exits with an error.

None of the datasets has two images of one subject (for example a T2w or CT to align to the
T1w), so the linear fits register a subject onto the template (`snsx`, the default) or one
subject onto another (`--dataset mindboggle` or `deep7t`); the fits are therefore large
corrections, not the small within-subject alignments of a coregistration.

**Post-processing (`post`).** For every pair whose registration finished, the composites of the
FastWarp registration (ANTs' when FastWarp has none; the same files for both tools) are turned
into:

| Step | FastWarp | ANTs |
| --- | --- | --- |
| forward field | `compose_field([Composite.h5], fixed reference)` | `antsApplyTransforms -r <fixed reference> -t Composite.h5 -o [<file>,1]` |
| inverse field | `compose_field([InverseComposite.h5], moving image)` | `antsApplyTransforms -r <moving image> -t InverseComposite.h5 -o [<file>,1]` |
| Lanczos re-warp | `apply_transforms(moving, fixed reference, [Composite.h5], interpolation="lanczosWindowedSinc")`, then writing the result with ANTsPy | `antsApplyTransforms -i <moving image> -r <fixed reference> -t Composite.h5 -n LanczosWindowedSinc -o <file>` |

The fixed reference is the cropped native grid of the fixed image (the evaluation grid), the
moving image the subject's original file. Before either tool runs, the four inputs (both images
and both composites) are read once, untimed, so that both tools find them in the operating
system's file cache; FastWarp runs first and would otherwise pay for the cold reads. Each time
covers reading the inputs and writing the output; ANTs runs with `--float 1`, so both write `float32`. The outputs are uncompressed `.nii`
files, so the times compare the computation and a raw write. With `--gzip` every step runs a
second time writing `.nii.gz`: `compose_field` compresses at gzip level 1 on all its threads,
ITK (ANTs' outputs, and FastWarp's re-warped image, which ANTsPy writes) at its default level
on one thread, so those rows measure the compression as much as the computation; the report
gives the output sizes. The step's time is the process wall time for an ANTs program
(`--ants-dir`), and for ANTsPy's bundled program the time around the library call inside the
child process (Python's start and `import ants` are not counted). The `.nii` outputs are
compared (field differences in mm, the relative RMS difference of the re-warped images), then
all outputs are deleted; only `post.json` stays. A pair whose comparison fails is recorded as
failed, without times. `post` redoes a pair, done or failed, when its registration was run
again since.

## Running it

### Install

The benchmark needs Python 3.13 (ANTsPy has no 3.14 wheels yet), ANTsPy and a GPU for FastWarp.
The `benchmark` dependency group adds `scipy` and `matplotlib` (both already pinned in `uv.lock`
as ANTsPy dependencies) to the `ants` group.

The benchmark is not part of the wheel. Run it from a clone of the repository, in the
environment that `uv.lock` pins (`uv sync --locked`); for results that you report, check out a
release tag first, so that others can run the same code.

```bash
git clone https://github.com/WyssCenter/fastwarp && cd fastwarp
export UV_PYTHON=3.13
uv sync --locked --no-install-project --group benchmark
# FastWarp of this checkout (needs the Rust toolchain of rust-toolchain.toml):
uv run --no-sync maturin develop --uv --release
# or the released wheel from PyPI, checked against its pinned hashes:
uv pip install --no-deps --require-hashes --only-binary :all: -r benchmarks/fastwarp-pypi.txt
uv run --no-sync python -c "import fastwarp; print(fastwarp.__version__, fastwarp.gpu_info())"
```

### Commands

Choose a data folder and a work folder outside the repository (the commands refuse folders
inside it). `--data` holds the downloads, `--work` the transforms, evaluations and reports.

```bash
B="uv run --no-sync python -m benchmarks.accuracy"
$B list                                                 # datasets, sizes, default pair counts
$B fetch --data ~/fastwarp-bench/data                   # download, verify, extract (4.4 GB)
$B run --quick --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work   # one pair per dataset
$B plan --work ~/fastwarp-bench/work                    # time left, from the measured --quick times
$B run --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work          # the full night
$B linear --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work       # linear fits (snsx)
$B post --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work         # fields, re-warp
$B evaluate --data ~/fastwarp-bench/data --work ~/fastwarp-bench/work
$B report --work ~/fastwarp-bench/work
```

- `run` resumes: a pair whose engine finished with the same recipe and grid is skipped, so an
  interrupted night continues where it stopped; a changed recipe or `--iso-mm` runs the pair
  again. `--retry-failed` reruns failed or skipped registrations. An error in one pair (a file
  that does not read, an engine that fails) is recorded in its `run.json` and the run goes on. A
  lock file stops two runs from sharing a work folder.
- `--dataset KEY` (repeatable) selects datasets, `--pairs N` the number of pairs, `--recipe
  syn-sh` the other recipe, `--engine fastwarp` or `--engine ants` one engine, `--threads N` a
  cap on the CPU threads of ANTs and of FastWarp's CPU work (default: all cores; see
  [Timings](#timings)), `--ants-dir DIR` an ANTs installation, `--adapter NAME` a GPU,
  `--iso-mm` the registration grid (default 1.0).
- `linear` takes the same selection options as `run` (default dataset `snsx`, the default-run
  dataset with landmarks; `--dataset` chooses others), `--engine`, `--threads`, `--ants-dir`
  and `--adapter`. It resumes like `run` and writes
  `<work>/<dataset>/linear/<pair>/{fastwarp,ants}/run.json` (times and fitted affines; ANTs'
  logs and `.mat` files beside them), `comparison.json` (landmark errors, agreement) and
  `<work>/environment_linear.json`. A changed recipe or `--iso-mm` fits the pair again.
- `post` post-processes the pairs `run` finished (same selection options and `--iso-mm`,
  `--threads`, `--ants-dir`, `--gzip`); it resumes, and writes
  `<work>/<dataset>/<recipe>/<pair>/post.json` and `<work>/environment_post.json`.
  `--ants-dir` must hold `antsApplyTransforms` as well as `antsRegistration`.
- `plan` estimates `run`, `evaluate`, `post` and `linear` together (`linear` on its default
  datasets unless `--dataset` is given).
- `evaluate` caches each pair's result in `evaluation.json` and writes `labels.csv` (one row per
  structure, pair and engine), `landmarks.csv`, `pairs.csv` and `agreement.csv` under
  `<work>/results/<recipe>/`; `--no-surface` skips HD95 and ASSD (a later run without it
  computes them). A pair that fails to evaluate is logged, recorded with its error in
  `evaluation.json` and tried again by the next `evaluate`; the other pairs go on.
- `report` writes `summary.md` (the tables to paste: accuracy when `evaluate` has run, and the
  registration, linear-fit and post-processing times with the machine that produced them),
  `paired.csv`, `agreement_summary.csv`, `structures.csv`, `timings.csv`, `linear.csv`,
  `post.csv` and, with matplotlib, `summary.png`. The timing tables are grouped by
  registration grid; `--iso-mm` keeps one grid. When the timing records come from more than one
  machine or software version, `summary.md` lists each with its number of records.
- `environment.json` (`run`), `environment_linear.json`, `environment_post.json` and every
  `run.json` and `post.json` record the versions (FastWarp and its engine revision, ANTsPy, the
  ANTs version from `antsRegistration --version` and the program used, NumPy, SciPy, Python),
  the operating system, the GPU (`fastwarp.gpu_info()`: name, backend, device type, and the
  driver name, which is the vendor's driver name, not a version), the CPU and its logical cores,
  the ITK and FastWarp thread counts, any `FASTWARP_*` variables, the benchmark's git commit and
  whether the checkout had local changes, the exact recipe, the registration grid and the ANTs
  argument list.

### Time and disk

These are estimates, not measurements; `plan` replaces them with the medians of the pairs
already run, so run `--quick` first and check `plan` before starting the night.

| Step | Assumption | Default run |
| --- | --- | --- |
| ANTs, one registration on a 1 mm grid, all cores of a desktop | about 400 s (a guess for one channel on the cropped, skull-stripped grids) | 50 registrations: about 5.6 h |
| FastWarp, one registration | about 40 s or less on a desktop GPU | about 0.6 h |
| Evaluation per pair | about 60 s | about 0.8 h |
| `linear`, one pair: ANTs rigid and affine; FastWarp's fits | about 120 s and 20 s | 20 `snsx` pairs: about 0.8 h |
| `post`, one pair, `.nii` outputs: ANTs; FastWarp (`--gzip` about doubles it) | about 90 s and 15 s | 50 pairs: about 1.5 h |
| Total (20 `mindboggle` + 10 `deep7t` + 20 `snsx` pairs) | | about 9.3 h; allow 10–12 h |

`--quick` (one pair per dataset) takes about 30 min without `linear` and `post`. If the night
is short, lower the pairs (`--pairs`, also per step) or
run the datasets or the steps on separate nights; every step resumes where it stopped.
`post` writes six uncompressed fields and images per pair (about 1 GB for a 7 T pair;
twice as many files with `--gzip`) and deletes them after comparing. Disk: 1.6 GB
of data after extraction, 4.5 GB at the peak (the 2.85 GB 7 T archive is deleted after
extraction unless `--keep-archives`), about 60 MB per pair in the work folder (transforms of both
engines), so about 3 GB for the default run. Memory: about 4 GB of RAM for ANTs and evaluation; the GPU
needs the memory `fastwarp.fits_gpu` checks (a pair that does not fit is recorded as skipped).

### Hardware notes

Run the night on a machine that does nothing else: ANTs' time depends on the CPU and on other
load, and the GPU time on other GPU work. Record the machine with the results (`report` prints
it). Leave `--threads` out for the timings to report; with it, say so next to the results.

## Supply chain

- Downloads use HTTPS only (redirects included) and stream into a `.part` file that becomes the
  file only when size and SHA-256 match the pin; a mismatch deletes it and stops. An interrupted
  download resumes with an HTTP range request.
- Archives are extracted member by member: only the members the dataset's rules select, only
  regular files, names and output paths checked to stay inside the data folder; links and
  devices are refused. Nothing is taken from the archive's permissions.
- `pins.json` was made by streaming every file through SHA-256 without storing it, and compared
  with the hashes the hosts publish: OSF's file metadata for the OSF files, the git-annex
  keys (`SHA256E-s<size>--<sha256>`) of the OpenNeuro datasets' version 1.0.1. A file with
  `"sha256": null` is a placeholder that `fetch` refuses. To pin a new or changed file:
  `uv run --no-sync python -m benchmarks.accuracy pin --dataset KEY` streams the unpinned files
  and prints their SHA-256 and size; compare them with the host's published checksum, then
  write them into `pins.json` by hand. `pin --all` re-checks every pin against the server; a
  stream whose length differs from the pinned size is refused.

## Results

To be added after the first full run, as the `summary.md` tables of each recipe with the
`environment.json` of the machine.

## References

- Avants BB et al. (2008). Symmetric diffeomorphic image registration with cross-correlation.
  *Med Image Anal* 12:26–41, doi:10.1016/j.media.2007.06.004.
- Klein A et al. (2009). Evaluation of 14 nonlinear deformation algorithms applied to human brain
  MRI registration. *NeuroImage* 46:786–802, doi:10.1016/j.neuroimage.2008.12.037.
- The dataset citations are listed with each dataset above.
