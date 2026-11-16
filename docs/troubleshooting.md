# Troubleshooting

If your problem is not listed here, see [Getting help](support.md).

## Installation and graphics card

**Windows warns before the installer runs ("Windows protected your PC").** The program is not yet
code-signed. Choose *More info*, then *Run anyway*.

**"No usable graphics driver found" or "Registration runs on the processor".** The graphics card
could not be used, so the application draws its windows and runs the registration on the
processor. Results are the same, but it is slower. Install the current driver from the graphics
card maker; the notice has a button that helps you find it. `dbsloc gpu probe` shows which
graphics card is found and whether it is used (`verdict: GO` means it is).

**The registration does not fit into the graphics card's memory.** With *Auto*, the step
continues on the processor and says so. With *GPU (wgpu)*, choose a coarser *Resolution* on the
*Normalize to template* card (or `--norm-isotropic-mm 1.0`), or use *ANTs (CPU)*.

## Error messages

**"no pre-op anchor" (`E_NO_ANCHOR`).** The subject has no pre-operative MRI in `ses-preop/anat`.
Import one.

**"post-op modality … was requested but … has no post-op … image" (`E_MISS_POSTOP`).** *Post-op
images to use* asks for a CT or MRI that the subject does not have. Set it to *Automatic*.

**"duplicate index rows" (`E_DUPL_RAW`).** Two entries in the subject's list of scans describe the
same kind of scan. Import again with *Replace existing images*, or remove the duplicate entry from
`rawdata/sub-<id>/rawimages.json`.

**"expected …/rawdata/sub-&lt;id&gt;" (`E_NO_SUBJ`) or "not a valid sub-&lt;id&gt; folder name".** The
subject does not exist in this dataset, or its id contains characters other than letters and
digits.

**"no electrode model set".** Choose the *Electrode model* on the *Reconstruct electrode* card, or
pass `--el-model "<name>"`. A name that is not in the list gives "is not in the electrode
catalogue"; the list in the application shows the exact spelling of every model.

**"qc not approved".** The run stopped at a registration that has not been approved (for example
in a run with `--headless`). Approve it in the *Registration review* tab or from the step's card,
then run the remaining steps.

**DICOM import fails (`E_DCM2NIIX`).** The converter could not read the folder. Make sure the
folder contains the DICOM files themselves (not a ZIP archive), and try one series folder at a
time.

## Electrodes

**A lead was not found.** If only one lead could be traced, the step still succeeds, warns that
the other lead "was not reconstructed and is left out of the file", and the reconstruction
contains one lead. Check the coregistration first (a misaligned CT is the most common cause), then
place the missing lead in *Refine localization*. For a post-operative MRI, try *Metal appearance:
Bright metal artefact*.

**A lead is in the wrong place or tilted strangely.** Look at the coregistration review: the CT's
skull must line up with the MRI. If it does not, reject it and run *Coregister* again, for example
with the other engine. Otherwise correct the lead in *Refine localization*
([Checking and correcting the electrodes](guide/correction.md)).

## The application closed unexpectedly

On the next start it says so and offers *Open log folder*. Please send us the log, after checking
it for patient information ([Getting help](support.md)).
