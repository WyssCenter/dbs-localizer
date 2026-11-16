# The processing steps

The pipeline runs its steps in a fixed order. On the command line they have these names (for
`dbsloc run --stages` and `dbsloc lock`):

`bids`, `preprocess`, `precoreg`, `coreg`, `normalize`, `brainshift`, `reconstruct`,
`orientation`, `vat`, `visualize`

`bids` finds the subject's scans in the dataset. `brainshift` (brain-shift correction) is not
implemented yet and is skipped. Each step uses the results of the steps before it; when a step is
computed again, the approvals of the steps after it are withdrawn ([Running steps
again](rerun.md)). The registration steps run on the graphics card or with the bundled ANTs
programs on the processor ([Engine](pipeline.md#engine)); `--preprocess-backend`,
`--coregister-backend` and `--normalize-backend` choose the engine per step on the command line.

The detailed behaviour of every step is written down in the specifications in
[`specs/`](../../specs), linked from each section below.

## Preprocessing

`preprocess` corrects slow brightness changes across the anchor scan with N4 bias-field
correction and writes `preprocessing/anat/sub-<id>_…_desc-preproc_T1w.nii.gz` (for a T1w
anchor). In the application it is the *Preprocess MRI / CT* card.
Specification: [`specs/02_preprocessing.md`](../../specs/02_preprocessing.md).

## Rigid alignment to the template

`precoreg` aligns the anchor rigidly with the template, which roughly places it in template
orientation; only the image header changes. Once its image exists, it is the anchor for the
coregistration, the normalization, the registration review and everything after. It needs the
template, like the normalization. It runs automatically before *Coregister* or *Normalize*;
`--no-coreg` drops it together with the coregistration.
Specification: [`specs/02_preprocessing.md`](../../specs/02_preprocessing.md) §6.

## Coregistration

`coreg` aligns every other scan of the subject rigidly to the pre-operative anchor: a pre-op T2w,
the post-operative CT and every post-operative MRI. For a CT several starting positions are
tried, so large head tilts are handled. The aligned scans are written to `coregistration/anat/`
with `space-anchorNative` in their names, the transforms to `coregistration/transformations/`,
and a record of how it was done to `coregistration/log/`. The run then pauses for the
[registration review](review.md).
Specification: [`specs/03_coregistration.md`](../../specs/03_coregistration.md).

## Normalization

`normalize` warps the anatomy non-linearly into template space with the SyN method of ANTs
(Avants et al. 2008), after rigid and affine stages. With a pre-op T2w and a T2 template, T1w and
T2w are registered together (*Multispectral (T1w + T2w)*); `--no-norm-multispectral` uses the T1w
only. The registration grid is by default the template's own voxel size (1 mm for the bundled
template); on the graphics card, a default grid that does not fit into its memory is made
coarser step by step, with a warning (a grid you chose is never changed). The normalized scans go to `normalization/anat/`; the forward and inverse
warps go to `normalization/transformations/` as `.h5` files, together with the linear part as
ITK `.mat` files. The run then pauses for the [registration review](review.md).

For a template in another space than MNI152NLin2009bAsym, the step also builds (once, and keeps)
a registration from the bundled template to it, so that the reconstruction can also be reported
in MNI152NLin2009bAsym space and the atlases can be shown on the chosen template.
Specification: [`specs/04_normalization.md`](../../specs/04_normalization.md).

## Reconstruction

`reconstruct` finds each lead and its contacts in the post-operative scan and writes them in the
subject's own (native) space and in template space. It needs the [electrode
model](concepts.md); the contacts are placed from the model's geometry. The *Method*
(`--recon-method`) chooses how the leads are found:

| Method | On a post-op CT | On a post-op MRI |
| --- | --- | --- |
| *Automatic* (`auto`, recommended) | a PaCER result exported beforehand, if one is present; else TRAC/CORE on the coregistered CT, with the points warped to the template | TRAC/CORE on the normalized scan in template space |
| `trac_core` | TRAC/CORE on the coregistered CT | TRAC/CORE on the normalized scan |
| `trac_core_native` | TRAC/CORE on the coregistered CT | refused: needs a CT |
| PaCER (`pacer`) | a PaCER result exported beforehand, if one is present; else PaCER (Husch et al. 2017) on the coregistered CT | refused: PaCER needs a CT |

PaCER is an optional component under its own licence; the release packages include it
([Licences](../licence.md)).

On a post-operative MRI in which the lead appears bright, *Metal appearance: Bright metal
artefact* (`--recon-bright-metal`) traces it as bright metal. If only one lead is found, the step
still succeeds with one lead and a warning.

Each lead is stored with its markers: *head* is the centre of the deepest contact level, *tail*
the 4th contact level from the bottom (the top level on leads with fewer levels). The result is
`reconstruction/sub-<id>_desc-reconstruction.json` and a Lead-DBS compatible
`sub-<id>_desc-reconstruction.mat` beside it ([Outputs](../outputs.md)). After the step, the 3D
scene is written and, when a display is available, opened. Check every lead and correct it where needed ([Checking and
correcting the electrodes](correction.md)).
Specifications: [`specs/05_reconstruction.md`](../../specs/05_reconstruction.md),
[`specs/09_electrode_models.md`](../../specs/09_electrode_models.md),
[`specs/10_pacer.md`](../../specs/10_pacer.md),
[`specs/12_reconstruction_mat_and_manual.md`](../../specs/12_reconstruction_mat_and_manual.md).

## Orientation of directional leads

`orientation` estimates the roll of a directional lead, its rotation about its own axis, from the
artefact of the orientation marker in the post-operative CT, following the DiODe approach. It is
**experimental**. It estimates Boston Scientific Vercise Directed, Vercise Cartesia HX and
Cartesia X and Abbott Directed 6172 / 6173; other directional models, Medtronic SenSight
included, are reported as not supported, and leads without segments are left alone. It needs a
post-operative CT; for a subject with a post-operative MRI the step is skipped. The result is
written into the reconstruction file. You can set or accept the roll by hand in *Refine
localization*.
Specification: [`specs/06_orientation.md`](../../specs/06_orientation.md).

## Volume of activated tissue

`vat` computes the electric field and the volume of activated tissue for the saved stimulation
settings of the subject, and is skipped when there are none. See [Stimulation and the volume of
activated tissue](stimulation.md).
Specification: [`specs/07_vat.md`](../../specs/07_vat.md).

## Visualization

`visualize` writes the 3D scene, `reconstruction/viz/scene_manifest.json`, that the [3D
viewer](viewer.md) opens. The same file is also written after every reconstruction.
Specifications: [`specs/08_visualization.md`](../../specs/08_visualization.md),
[`specs/11_atlases.md`](../../specs/11_atlases.md).

The order of the steps, the approvals and what a run reports are specified in
[`specs/00_overview.md`](../../specs/00_overview.md).
