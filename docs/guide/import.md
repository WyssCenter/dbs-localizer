# Importing scans

The import converts DICOM folders with the bundled dcm2niix, suggests for each scan its session
(pre-operative or post-operative) and type, and writes the scans into the subject's `rawdata/`
folder of a [BIDS](https://bids.neuroimaging.io) dataset.

## In the application

Choose *Import… → Images into a subject…* in the header, *Import raw NIfTI / DICOM…* on the start
screen, or press `Ctrl+I`. The import window has three pages:

1. **Images.** Click *Add folders…* for DICOM folders or *Add NIfTI files…* for `.nii` /
   `.nii.gz` files. DICOM series are converted with the bundled dcm2niix, one folder at a time.
   Each series becomes a row with a suggested *Session* (pre-op or post-op) and *Type* (T1w, T2w,
   CT, …). Check every row and correct it where needed; rows the software could not classify are
   left unticked. *View* opens a quick preview of a series. When the folders contain scans from
   several dates, *Post-op = latest study* marks the newest study as post-operative.
2. **Subject and destination.** Enter the *Subject ID* (it is suggested from the folder name) and
   the *Dataset folder*. *Keep a copy of the source DICOM in sourcedata* (ticked by default)
   copies the original DICOM files into the dataset, under `sourcedata/sub-<id>/DICOM/`. *Replace
   existing images* is needed only when the subject already has scans of the same kind. If you
   tick both a post-op CT and a post-op MRI, choose which one the pipeline uses.
3. **Review.** Shows exactly which files will be written. Nothing is written until you click
   *Import*. An import without a pre-operative anchor scan is refused.

After a successful import, *Open subject* takes you to the new subject.

To import many patients at once, tick *One subject per input folder* on the first page: each
folder you add becomes its own subject.

You cannot import while something else may write into a subject: a run or a run of several
subjects, an open *Registration review* (or the clean-up that runs for a moment after it), an open
*Refine localization* (or a correction being saved), or *Compute VTAs* in the group analysis. The
import buttons are then greyed out; hover over them to see why. If one of these starts while the
import window is open, *Import* is greyed out and your choices are kept; import when it has
finished. The other way round, while an import is writing files into the dataset, runs,
registration reviews, lead corrections and the group's *Compute VTAs* wait or are refused with a
message. Closing the application then asks first: *Keep importing*, *Cancel the import, then quit*
(the subject being written is restored), or *Quit now*; if the import finishes, or is cancelled
and fully undone, while the question is open, the application closes. If the import fails instead,
the application stays open and shows the import window with the error, so you can see what went
wrong. Saving stimulation settings, *Compute connectivity*, the group's analyses and importing a
session report are not affected: they do not touch the imported images.

## On the command line

```bash
dbsloc import my-study --subject 01 --dicom path/to/dicom-folder --dry-run    # show the plan, write nothing
dbsloc import my-study --subject 01 --dicom path/to/dicom-folder --auto       # import the suggested scans
dbsloc import my-study --subject 01 --nifti t1.nii.gz --nifti ct.nii.gz --item preop:T1w=t1.nii.gz --item postop:CT=ct.nii.gz
```

`--item SESSION:TYPE=SOURCE` assigns a scan by hand; `SOURCE` is a file name or `series:<number>`
for a converted DICOM series. `dbsloc import --help` lists every option.

Without `--auto`, scans that could not be assigned stop the import with exit code 2; `--auto`
leaves them out. `--postop-latest-study` turns the assigned pre-operative MRI series of the
latest study into post-operative ones when the folders hold two or more dated studies. The default subject id is the
name of the first input folder (a folder named `DICOM` uses its parent), cleaned to letters and
digits. Exit codes: 0 imported, 1 other errors, 2 usage and dataset errors (`E_BIDS_PARSE`,
`E_NO_ANCHOR`, `E_DUPL_RAW`, `E_NO_SUBJ`), 130 stopped with `Ctrl+C`.

## Patient data

dcm2niix leaves patient names, ids and birth dates out of the NIfTI files and their sidecars. The
original DICOM files still contain them; if the dataset must not contain identifying data, untick
*Keep a copy of the source DICOM in sourcedata* (command line: `--no-keep-sourcedata`).

## Which post-operative scan is used

When a subject has both a post-operative CT and a post-operative MRI, the pipeline uses the CT,
unless the subject's Lead-DBS settings (`uiprefs.mat`) chose the MRI. *Post-op images to use* on
the *Coregister post-operative image* card, or `dbsloc run --postop-modality ct|mri`, forces
one.

The import rules, the suggestion table and the BIDS names are specified in
[`specs/01_bids.md`](../../specs/01_bids.md) and
[`specs/13_import_and_batch_reference.md`](../../specs/13_import_and_batch_reference.md).
