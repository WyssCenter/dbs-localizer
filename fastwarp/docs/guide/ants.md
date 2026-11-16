# ANTsPy compatibility

`fastwarp.ants` takes the calls of [ANTsPy](https://github.com/ANTsX/ANTsPy) that register and
resample images, with the same arguments and the same results, for the part of ANTsPy that
FastWarp supports. A script that uses `ants.registration` and `ants.apply_transforms` can move
its registrations to the GPU by calling the functions of `fastwarp.ants` instead:

```python
import ants
import fastwarp.ants

fixed = ants.image_read("template.nii.gz")
moving = ants.image_read("sub-01_T1w.nii.gz")

reg = fastwarp.ants.registration(fixed, moving, type_of_transform="SyNCC", outprefix="out/sub-01_")
warped = fastwarp.ants.apply_transforms(fixed, moving, transformlist=reg["fwdtransforms"])
```

The transform files have ANTs' names, so `ants.apply_transforms` and `antsApplyTransforms` read
them as well. The two functions follow ANTsPy 0.6.3 and need ANTsPy itself (the `antspyx`
package) for their `ANTsImage` input and output; `import fastwarp.ants` does not import it.

## How a call runs

{func}`fastwarp.ants.registration` builds the `antsRegistration` argument list that ANTsPy
builds for the same arguments, token for token (the tests compare the two for every supported
`type_of_transform`), and runs it with the [ANTs compatibility mode](../cli.md#ants-registration)
of the command line: the same code as `fastwarp ants-registration`, with the images handed over
in memory instead of files. {func}`fastwarp.ants.apply_transforms` does the same with
`antsApplyTransforms`, on the CPU.

Three rules hold for every argument:

- **Refused before any work.** An argument that would change the result and that FastWarp does
  not support raises {class}`fastwarp.ants.UnsupportedArgument` (a `ValueError`) before the GPU
  is opened and before any transform file is written (the output folder may already exist). The message names the argument, its value and the reason:
  `fastwarp.ants.registration: syn_metric='mattes' is not supported: ...`.
- **Ignored only where ANTsPy ignores it.** ANTsPy ignores some arguments for some types (for
  example `aff_iterations` for `SyNCC`); FastWarp ignores them too and warns with a
  `UserWarning`.
- **No silent fallback.** Unknown keyword arguments raise `TypeError` (ANTsPy swallows them).
  With `fallback="ants"`, a refused argument, a machine without a usable GPU, or a registration
  that does not fit the GPU runs ANTsPy's own function with the same arguments instead, with a
  `UserWarning`; the result then has an `"engine"` key, `"fastwarp"` or `"ants"`. Without it,
  {class}`~fastwarp.GpuUnavailableError` and {class}`~fastwarp.GpuCapacityError` are raised.

## Types of transform

| `type_of_transform` | In FastWarp |
| --- | --- |
| `Rigid`, `Affine`, `AffineFast`, `BOLDAffine`, `QuickRigid`, `DenseRigid`, `BOLDRigid` | Runs. |
| `SyNCC`, `SyNabp` | Runs. |
| `antsRegistrationSyN[r]`, `[a]`, `[s]`, `[sr]`, `[so]` (and without brackets, `[s]`) | Runs. ANTsPy 0.6.3 uses CC radius 2 even when the type names another (`[s,4]`), and so does FastWarp, with a warning. |
| `antsRegistrationSyNQuick[r]`, `[a]` | Runs. |
| `SyN`, `SyNRA`, `SyNOnly`, `SyNAggro`, `SyNLessAggro`, `ElasticSyN`, `SyNBold`, `SyNBoldAff` | Runs with `syn_metric="CC"` and `syn_sampling` (the CC radius) from 1 to 8. With ANTsPy's default `syn_metric="mattes"` it is refused: FastWarp's SyN stage has no Mattes mutual information metric. **The default call `registration(fixed, moving)` is therefore refused; use `"SyNCC"`, or `syn_metric="CC"`.** |
| `antsRegistrationSyNQuick[s]`, `[sr]`, `[so]` | Refused: Mattes mutual information in the SyN stage. |
| `Translation`, `Similarity`, `TRSAA`, `antsRegistrationSyN[t]` | Refused: no Translation or Similarity transform. |
| `antsRegistrationSyN[b]`, `[br]`, `[bo]` | Refused: no BSplineSyN transform. |
| `antsRegistrationSyNRepro[...]`, `antsRegistrationSyNQuickRepro[...]` | Refused: no GC metric. |
| `Elastic`, `TV[1]` to `TV[8]`, `TVMSQ`, `TVMSQC` | Refused: no Gaussian displacement field or time-varying velocity field transform. |

Any other name raises ANTsPy's `ValueError("... does not exist")`.

## Arguments of `registration`

| Argument | In FastWarp |
| --- | --- |
| `fixed`, `moving` | `ANTsImage`, 3-D and scalar; other dimensions or vector images are refused. ANTsPy's own checks (NaN voxels, different dimensions) raise ANTsPy's messages. |
| `initial_transform` | `None` (centres of intensity mass, or the identity for `SyNOnly` and `antsRegistrationSyN[so]`), `"Identity"`, or one file holding one affine transform (`.mat`, `.txt`, or an `.h5` such as the `Composite.h5` of a linear run; also as `"[file,1]"` for its inverse). A displacement field, more than one file, or ANTs' `"[fixed,moving,feature]"` form with file names is refused. |
| `outprefix` | Text joined to the file names, as in ANTsPy; the folder is created when missing. `""` makes a private temporary folder (`tempfile.mkdtemp`) and the prefix `fw_` in it, where ANTsPy uses the race-prone `tempfile.mktemp`. |
| `mask` | A fixed-image mask (non-zero voxels inside) for types whose last stage is linear. ANTsPy puts the mask on the last stage, so with a SyN stage it is refused: FastWarp's SyN stage has no mask. |
| `moving_mask` | Refused. |
| `mask_all_stages` | As in ANTsPy (with a mask only linear types run, where it changes nothing). |
| `grad_step`, `flow_sigma`, `total_sigma` | The SyN stage's step and field variances, for the types that use them. |
| `aff_metric` | `"mattes"` (or `"MI"`); `"GC"` and `"meansquares"` are refused. |
| `aff_sampling` | `32` only (the GPU's histogram bins). |
| `aff_random_sampling_rate` | Any value in (0, 1]; samples on a regular grid, as ANTsPy asks. |
| `syn_metric`, `syn_sampling` | `"CC"` with a radius from 1 to 8. |
| `reg_iterations` | 1 to 10 levels; shrink factors and sigmas follow as in ANTsPy. |
| `aff_iterations`, `aff_shrink_factors`, `aff_smoothing_sigmas` | As in ANTsPy for the linear types. A list is taken like a tuple; ANTsPy builds a broken argument from a list. |
| `write_composite_transform` | `Composite.h5` and `InverseComposite.h5` only, as ANTs writes them. |
| `verbose` | Prints the `antsRegistration` arguments (images by their argument names) and FastWarp's report of the run. |
| `multivariate_extras` | One `("CC", fixed2, moving2, weight, radius)` with the main CC radius, for `SyNOnly` and `antsRegistrationSyN[s|sr|so]`: a second channel. `fixed2` must lie on `fixed`'s grid. More extras or other metrics are refused. |
| `restrict_transformation` | All ones only (no restriction). |
| `smoothing_in_mm` | As in ANTsPy, for the linear types. |
| `singleprecision` | Accepted; the GPU computes in `float32` either way. `True` (the default) writes `float32` warp fields, `False` `float64`, as ANTs does. |
| `use_legacy_histogram_matching` | `True` is refused (no histogram matching). |
| `ants.set_ants_deterministic(seed_value=...)` | The seed of the linear stages' sampling, as in ANTsPy; without it `ANTS_RANDOM_SEED`, else FastWarp's fixed seed. |

FastWarp adds four keyword-only arguments: `iso_mm` (the voxel size of the
{term}`registration grid`, by default the fixed image's smallest spacing), `adapter` (the GPU),
`cancel` (a {class}`~fastwarp.CancelToken`) and `fallback`.

## What `registration` returns

The dict ANTsPy returns, with the same keys:

| Key | Value |
| --- | --- |
| `warpedmovout` | `moving` resampled onto `fixed`'s grid (linear interpolation), an `ANTsImage` with `fixed`'s pixel type. |
| `warpedfixout` | `fixed` resampled onto `moving`'s grid through the inverse, also with `fixed`'s pixel type, as in ANTsPy. |
| `fwdtransforms` | `[prefix + "1Warp.nii.gz", prefix + "0GenericAffine.mat"]` after a SyN stage, `[prefix + "0GenericAffine.mat"]` for linear types, or the string `prefix + "Composite.h5"` with `write_composite_transform=True`. |
| `invtransforms` | `[prefix + "0GenericAffine.mat", prefix + "1InverseWarp.nii.gz"]`, `[prefix + "0GenericAffine.mat"]`, or the string `prefix + "InverseComposite.h5"`. |

The lists come from the files this call wrote, not from a search of the folder as in ANTsPy, so
older files under the same prefix are never picked up. Both lists go straight into
`apply_transforms`, with the same rule as in ANTsPy for inverting the affine: after a SyN stage
the default `whichtoinvert` of `invtransforms` inverts it; for a linear type pass
`whichtoinvert=[True]`.

## Arguments of `apply_transforms`

| Argument | In FastWarp |
| --- | --- |
| `fixed` | The output grid and pixel type, an `ANTsImage`. ANTsPy's form with file names (`fixed` as a string) is refused. |
| `moving` | A 3-D scalar `ANTsImage`. |
| `transformlist` | As in ANTsPy; a missing file raises `FileNotFoundError` with ANTsPy's message. |
| `interpolator` | `"linear"`, `"nearestNeighbor"`, `"lanczosWindowedSinc"`. `"genericLabel"`, `"multiLabel"`, `"gaussian"`, `"bSpline"` and the other windowed sincs are refused. |
| `imagetype` | `0` only. |
| `whichtoinvert` | As in ANTsPy, with its default (`(True, False)` for exactly two files of which only the first is a `.mat` file, nothing inverted otherwise) and its checks. |
| `compose` | Writes `compose + "comptx.nii.gz"` (or the `.h5` file it names) and returns its name, `float64` unless `singleprecision=True`, as ANTs writes it. |
| `defaultvalue`, `verbose` | As in ANTsPy. |
| `singleprecision` | Sets the type of a composed field (`float64` by default, `float32` with `True`). Resampled images are computed in `float32` either way, where ANTsPy computes in `float64` by default, and then cast to `fixed`'s pixel type. |

## Argument lists

`registration(["-d", "3", ...], None)`, ANTsPy's way to pass `antsRegistration` arguments
directly, runs them exactly as `fastwarp ants-registration` does and returns 0: the files it
writes hold the same data as those of the command line on the same arguments. Images are named by
their files; the other arguments of ANTsPy have no effect there (a warning says so), and
`iso_mm`, `adapter`, `cancel` and `fallback` work as above. This form needs no ANTsPy (except
for `fallback="ants"`).

## Differences to ANTs

The results agree closely with ANTs but are not identical. The main reasons, which
[Troubleshooting](../troubleshooting.md#results-differ-slightly-from-ants) explains further:

- The images are registered on an isotropic {term}`registration grid` at the fixed image's
  smallest spacing, and the moving image is resampled onto it. With an isotropic fixed image
  this grid is the fixed image's own; with an anisotropic one the warp fields are resampled onto
  the fixed image's grid when they are written.
- The GPU computes in `float32`; ANTs computes in `float32` with ANTsPy's default
  `singleprecision=True`, in `float64` otherwise.
- The sampling of the linear stages follows ITK's random generator but starts from a fixed seed,
  so two runs on the same GPU give the same result; ANTs uses the clock unless a seed is set.
- Transform files carry the type names `*_double_3_3`, which every ANTs reader accepts.
- **Levels without usable samples are skipped, where ANTs drops the whole stage.** When a level of a Rigid or Affine stage gets no sample point on the fixed image, or only points with one fixed value (typically a coarse level on a small image or inside a small mask: at a shrink factor of 6 a 32-voxel axis has 5 voxels, the regular sampling stride with 20 % of the voxels is 5 as well, and every candidate falls on the same mask edge), Mattes mutual information is undefined there. ANTs 2.6.5 then throws inside the stage, catches the exception, drops the whole stage and exits with status 0, so only the start transform remains (ANTsPy returns it without notice). FastWarp skips only that level, keeps the parameters, runs the finer levels and warns (a `UserWarning` naming the stage, the level, its shrink factors and
  the number of candidate points). When every level of a stage is skipped, the call fails with
  {class}`~fastwarp.RegistrationError` and names the remedies: smaller shrink factors, a larger
  `aff_random_sampling_rate`, a larger mask, or a smaller `iso_mm`. This is better than ANTs,
  but a masked result on a small image can then differ from ANTsPy's by the whole stage.
- A few corner cases are handled more strictly than ANTsPy: `compose=""` raises `ValueError`
  (ANTsPy writes a field called `NA`); `None` entries of a raw argument list are left out
  (ANTsPy's `process_arguments` repeats the entry before it, or fails on a leading `None`), and an
  `ANTsImage` in it is refused; and
  `smoothing_in_mm=True` with integer `aff_smoothing_sigmas` works (ANTsPy fails on `int + str`).

The coarse levels of the linear types see few voxels on small images (ANTsPy's `Rigid` schedule
shrinks by 6 and samples 20 % of the voxels), so on small test images both ANTs and FastWarp can
end in a different local optimum depending on the seed.
