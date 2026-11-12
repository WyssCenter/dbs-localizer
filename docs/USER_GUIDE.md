# Wyss DBS Localizer user guide

This guide explains how to use Wyss DBS Localizer, from your first import to group analyses. It is written for researchers who use the software. You do not need to know how to program.

Installation is described in the [README](../README.md#installation). Words in *italics* are labels you see on screen; `code` is something you type.

## Contents

1. [Basic ideas](#1-basic-ideas)
2. [Importing scans](#2-importing-scans)
3. [The main window](#3-the-main-window)
4. [Running the pipeline](#4-running-the-pipeline)
5. [Checking registrations](#5-checking-registrations)
6. [Checking and correcting the electrodes](#6-checking-and-correcting-the-electrodes)
7. [The 3D viewer](#7-the-3d-viewer)
8. [Stimulation and the volume of activated tissue](#8-stimulation-and-the-volume-of-activated-tissue)
9. [Importing Medtronic Percept session reports](#9-importing-medtronic-percept-session-reports)
10. [Several subjects at once](#10-several-subjects-at-once)
11. [Group analysis](#11-group-analysis)
12. [Connectomes](#12-connectomes)
13. [Templates and backdrop images](#13-templates-and-backdrop-images)
14. [Using the command line](#14-using-the-command-line)
15. [Where results are stored](#15-where-results-are-stored)
16. [Running steps again](#16-running-steps-again)
17. [Checking the accuracy yourself with Lead-Tutor](#17-checking-the-accuracy-yourself-with-lead-tutor)
18. [Troubleshooting](#18-troubleshooting)
19. [Getting help](#19-getting-help)

## 1. Basic ideas

**Dataset.** All scans of a study live in one folder in the [BIDS](https://bids.neuroimaging.io) layout, the layout Lead-DBS also uses. Scans go under `rawdata/`; everything Wyss DBS Localizer computes goes under `derivatives/leaddbs/`. You never have to edit `rawdata/` by hand: the import does it for you.

**Subject.** One patient, stored as `sub-<id>`, for example `sub-01`. The id contains only letters and digits.

**Pre-operative and post-operative scans.** The pre-operative MRI (T1-weighted, and T2-weighted if you have one) shows the anatomy. The post-operative CT or MRI shows the implanted leads. The first pre-operative scan, usually the T1w, is the **anchor**: every other scan of the subject is aligned to it.

**Template space.** To compare patients and to use a brain atlas, the anchor is warped to a standard brain, the MNI152NLin2009bAsym template ("MNI space"). The bundled template has 1 mm voxels; others can be downloaded.

**Steps.** The pipeline runs a fixed sequence of steps: preprocessing, alignment of the scans, normalization to the template, electrode reconstruction, orientation of directional leads, the volume of activated tissue, and the 3D scene. You choose which steps run. Each step uses the results of the steps before it.

**Approval.** When you have checked the leads (corrected or accepted each one), the reconstruction is approved, and later command-line runs keep it instead of computing it again. Reviewing a registration records your verdict, but does not protect the registration from being computed again. See [Running steps again](#16-running-steps-again).

**Electrode model.** The reconstruction needs to know which lead was implanted (for example *Medtronic 3389* or *Boston Scientific Vercise Directed*). 73 models are supported. If the subject was set up in Lead-DBS before, the model stored there is used; otherwise you choose it.

## 2. Importing scans

### In the application

Choose *Import… → Images into a subject…* in the header, *Import raw NIfTI / DICOM…* on the start screen, or press `Ctrl+I`. The import window has three pages:

1. **Images.** Click *Add folders…* for DICOM folders or *Add NIfTI files…* for `.nii` / `.nii.gz` files. DICOM series are converted with the bundled dcm2niix, one folder at a time. Each series becomes a row with a suggested *Session* (pre-op or post-op) and *Type* (T1w, T2w, CT, …). Check every row and correct it where needed; rows the software could not classify are left unticked. *View* opens a quick preview of a series. When the folders contain scans from several dates, *Post-op = latest study* marks the newest study as post-operative.
2. **Subject and destination.** Enter the *Subject ID* (it is suggested from the folder name) and the *Dataset folder*. *Keep a copy of the source DICOM in sourcedata* (ticked by default) copies the original DICOM files into the dataset, under `sourcedata/sub-<id>/DICOM/`. *Replace existing images* is needed only when the subject already has scans of the same kind. If you tick both a post-op CT and a post-op MRI, choose which one the pipeline uses.
3. **Review.** Shows exactly which files will be written. Nothing is written until you click *Import*. An import without a pre-operative anchor scan is refused.

After a successful import, *Open subject* takes you to the new subject.

To import many patients at once, tick *One subject per input folder* on the first page: each folder you add becomes its own subject.

You cannot import while something else may write into a subject: a run or a run of several subjects, an open *Registration review* (or the clean-up that runs for a moment after it), an open *Refine localization* (or a correction being saved), or *Compute VTAs* in the group analysis. The import buttons are then greyed out; hover over them to see why. If one of these starts while the import window is open, *Import* is greyed out and your choices are kept; import when it has finished. The other way round, while an import is writing files into the dataset, runs, registration reviews, lead corrections and the group's *Compute VTAs* wait or are refused with a message. Closing the application then asks first: *Keep importing*, *Cancel the import, then quit* (the subject being written is restored), or *Quit now*; if the import finishes, or is cancelled and fully undone, while the question is open, the application closes. If the import fails instead, the application stays open and shows the import window with the error, so you can see what went wrong. Saving stimulation settings, *Compute connectivity*, the group's analyses and importing a session report are not affected: they do not touch the imported images.

### On the command line

```bash
dbsloc import my-study --subject 01 --dicom path/to/dicom-folder --dry-run    # show the plan, write nothing
dbsloc import my-study --subject 01 --dicom path/to/dicom-folder --auto       # import the suggested scans
dbsloc import my-study --subject 01 --nifti t1.nii.gz --nifti ct.nii.gz --item preop:T1w=t1.nii.gz --item postop:CT=ct.nii.gz
```

`--item SESSION:TYPE=SOURCE` assigns a scan by hand; `SOURCE` is a file name or `series:<number>` for a converted DICOM series. `dbsloc import --help` lists every option.

**Patient data.** dcm2niix leaves patient names, ids and birth dates out of the NIfTI files and their sidecars. The original DICOM files still contain them; if the dataset must not contain identifying data, untick *Keep a copy of the source DICOM in sourcedata* (command line: `--no-keep-sourcedata`).

## 3. The main window

The start screen lists your *Recent subjects* and *Recent folders* and offers *Open BIDS folder…*, *Open BIDS subject…* and *Import raw NIfTI / DICOM…*. Tick *Guide me through the steps* to go through a run one step at a time (see below), or click *Skip* to go to the full workspace.

The workspace has these tabs. You can drag them to arrange the window; *Settings → Reset panel layout* restores the original arrangement.

| Tab | Shortcut | Contents |
| --- | --- | --- |
| *Subjects* | `Ctrl+1` | the subjects of the dataset; tick several to run them one after another |
| *Pipeline* | `Ctrl+2` | the steps, their options and the *Run pipeline* button |
| *Output* | `Ctrl+3` | messages of the running steps |
| *3D Viewer* | `Ctrl+4` | leads, atlas and slices in 3D |
| *Registration review* | `Ctrl+5` | overlays to check each registration |
| *Refine localization* | `Ctrl+6` | manual correction of the leads |
| *Stimulation* | | the stimulation editor |
| *Group analysis* | `Ctrl+7` | group analyses |

*Registration review*, *Refine localization*, *Stimulation* and *Group analysis* appear beside the *3D Viewer* when you need them and can be closed (the ✖ on the tab) while they hold nothing; nothing of them is restored at the next start.

Above the tabs, two rows of the same shape pick the *Input directory* (the BIDS dataset, with *Recent* and *Browse…*, `Ctrl+O`) and the *Output directory*. Both fields are editable. By default the output directory is empty and its grey hint shows where results go (`derivatives/leaddbs` inside the input directory); type or browse to put them somewhere else, for example to keep the dataset read-only, and clear the field or choose *Default* in *Recent* to go back.

The header has *Home* (`Ctrl+H`, back to the start screen), *Import…*, *Connectomes…* and *Group analysis* (`Ctrl+7`) on the left, and *Settings* (colour theme, *Reset panel layout*) and *Help* (`F1`, shortcuts and system information) on the right. The subject list and the step outputs are scanned again whenever the window regains focus, and on `F5`.

**Guided run.** The guide walks through *Subject*, *Preprocess*, *Coregister*, *Normalize*, *Reconstruct*, *Stimulation* and *Review*. On each step you can *Run this step*, *Use existing* results, *Skip* it, or go *Back*. *Stimulation* is optional: with a saved stimulation it offers *Compute VTA* for the one you choose (shown in the 3D viewer from *Review*), otherwise it reads *Skip*. *Switch to full view* leaves the guide at any time.

## 4. Running the pipeline

Select a subject in *Subjects*. The *Pipeline* tab shows one card per step. Tick a card to include the step; its *Options* then unfold. The arrow on a card folds it to one row; *Collapse all* / *Expand all* folds every card. *Select all steps* and *Clear selection* tick or untick all of them.

| Card | What the step does | Main options |
| --- | --- | --- |
| *Preprocess MRI / CT* | Corrects slow brightness changes across the anchor scan (N4 bias-field correction). Before *Coregister* or *Normalize*, the anchor is also roughly aligned with the template, automatically. | *Engine* |
| *Coregister post-operative image* | Aligns the post-operative CT or MRI to the pre-operative anchor. Several starting positions are tried, so large head tilts are handled. | *Engine*; *Post-op images to use* (*Automatic* takes the CT when both a CT and an MRI exist) |
| *Normalize to template* | Warps the anatomy non-linearly into template space (ANTs SyN). With a pre-op T2w, T1w and T2w are registered together, which is more accurate. | *Template*; *T2 template*; *Multispectral (T1w + T2w)*; *Resolution* (*Template resolution* is recommended); *Engine* |
| *Reconstruct electrode* | Finds each lead and its contacts in the post-operative scan and maps them to template space. | *Electrode model* (required); *Method* (*Automatic* is recommended); *Metal appearance* (for post-op MRI: try *Bright metal artefact* if the lead is not found) |
| *Estimate directional orientation* | Estimates the rotation of directional leads from the CT artefact of their orientation marker. Experimental; Boston Scientific and Abbott leads only. | |
| *Volume of activated tissue* | Computes the electric field and the VTA for the saved stimulation settings; skipped when there are none. See [section 8](#8-stimulation-and-the-volume-of-activated-tissue). | *Edit stimulation…*; *Import session report…* (always visible on the card) |
| *Write electrode image* | Writes the 3D scene the viewer opens. | *Lead-DBS atlases directory* |

**Engine.** *Auto (GPU when available)* uses the graphics card when it passes a short self-test at start-up, and the bundled ANTs on the processor otherwise. *GPU (wgpu)* insists on the graphics card; *ANTs (CPU)* always uses the processor. Both reach the same accuracy on the validation data; the graphics card is many times faster.

**Electrode model.** Choose the implanted model in the list, or *From each subject's settings* to use the model stored by Lead-DBS for each subject. Without a model, the reconstruction cannot run.

**Run options** (below the cards):

- *Number of threads*: how many processor cores the processor-based steps may use. Lower it if you need the computer for other work during a run.
- *Rerun all selected steps*: compute ticked steps again even if results exist. Without it, steps with existing results are skipped (see [Running steps again](#16-running-steps-again)).
- *Review registrations*: when to check registrations (see [section 5](#5-checking-registrations) and [section 10](#10-several-subjects-at-once)).
- *Stop on first error*: skip the remaining steps of a subject once one of its steps fails.

Each card shows whether it can run: ✔ ready, ⚠ a warning, ✖ something is missing (for example *Needs a normalized post-operative image*). If a ticked step cannot run, the panel lists the reasons under *Before this run can start*, and *Untick the steps that cannot run* removes them.

Click *▶ Run pipeline* (`Ctrl+R`). The cards show *queued*, *running*, *done*, *skipped* (with the reason) or *failed*; the *Output* tab shows the details. *⏹ Stop* (`Ctrl+.`) stops the run. *Show results* opens the subject's results in the file manager.

A full run with the default settings takes about one minute per subject with a graphics card, and considerably longer on the processor alone.

*Subject images* at the top of the panel lists the subject's scans; *View* opens any of them in a small viewer, which is useful to check that the right scan was imported.

## 5. Checking registrations

A wrong registration makes everything after it wrong, so the software asks you to look at each one. With *Review registrations* set to its default, the run pauses after *Coregister* and after *Normalize* and opens the *Registration review* tab (in the guided run, the review appears inside the guide).

Each image shows the moving scan over the fixed one, for example the post-op CT over the pre-op MRI, or the patient's MRI over the template. Check that edges, ventricles and the skull line up.

- Approve each image with `A` and reject it with `D`; both move on to the next image. On the last image the buttons read *Approve & continue run* and *Reject & stop run*.
- One rejected image stops the run and marks the step *needs rework*. Change the settings of that step (for example the engine, the template or the resolution) and click *Rerun step* on its card.
- Arrow keys move between images, `Page Up` / `Page Down` between slices; `1`, `2`, `3` show one plane, `0` all three; `R` resets the zoom. *Window / level* offers presets for soft tissue and bone.

You can review a finished registration later with *Review registration* on its card. The cards show *Reviewed ✔*, *Rejected* or *Not reviewed*. Later steps warn when they use a registration that is rejected or not reviewed, but they still run.

## 6. Checking and correcting the electrodes

After *Reconstruct*, the Reconstruct card shows one line per lead: whether it was found, corrected or accepted. Always look at the result in the 3D viewer and in *Refine localization*.

Click *Refine localization* on the Reconstruct card. The view shows the post-operative scan in planes along the lead, with the lead model drawn on top.

- Select the deepest contact with `H` (*Head*) or the top end of the contact array with `T` (*Tail*), then drag it, or move it with the arrow keys (0.25 mm per press; with `Shift` 1 mm, with `Ctrl` 0.05 mm).
- *Keep lead length* (`K`) keeps the distance between head and tail fixed.
- For directional leads, set the *Roll* with the slider or `[` / `]`. *Flip 180°* and *Other candidate* switch between the orientations the automatic estimate found; *Automatic* returns to the estimate.
- *Accept side* (`Space`) marks a lead as checked without changing it.
- *Undo* / *Redo* (`Ctrl+Z` / `Ctrl+Y`) work per lead.
- *Save corrections* (`Ctrl+Enter`) writes them to the reconstruction; *Discard* (`Esc`) throws them away.

*Refine localization* follows the subject you select in the subject list. With unsaved corrections it stays on its subject and asks: *Save and switch to sub-<id>*, *Discard and switch to sub-<id>* or *Stay on sub-<id>*. While a correction is being saved, it switches once the save has finished. A subject without a reconstruction is shown as such. Opening the lead of the newly selected subject never holds back a run or an image import.

The contacts always follow the electrode model: you place the two ends, the software places the contacts in between. A corrected lead shows *● Manually corrected* on the card. Once every lead is corrected or accepted, the reconstruction counts as approved, and later runs keep it. Results that depend on the leads, such as the volume of activated tissue and the 3D scene, must be computed again; the message after saving says which.

On the command line, `dbsloc reco show my-study 01` prints the lead positions and `dbsloc reco correct` moves a lead (`dbsloc reco correct --help`).

## 7. The 3D viewer

The *3D Viewer* tab shows the leads, their contacts, slices of a background image and the structures of an atlas, all in template space. After a run it opens the subject's scene automatically.

**Mouse.** Left drag rotates; right drag (or `Ctrl` + left drag) moves; the wheel or middle drag zooms; double click resets the view. This is the same as in Lead-DBS. In the side panel's *View* section, *Mouse & touchpad* switches to the *Classic* scheme. Touchpad pinch and two-finger scrolling work too.

**Side panel.**

- *Slices*: choose the *Backdrop* image (the template, the subject's normalized scans, a downloaded high-resolution image, or *Choose file…*), and set *Index* and *Opacity* of the axial, coronal and sagittal slices. `1`, `2`, `3` show or hide them.
- *Window / level*: brightness and contrast of the slices.
- *Electrode*: *Show lead bodies*, *Mark and number contacts*.
- *Stimulation*: *Show VTA* draws the volume of activated tissue.
- *Fibres*: one row per fibre layer with a tick box, a colour swatch (click it to change the colour of a file you added), *n of N fibres shown* and *Remove*. *Width* sets the line width in pixels; *Maximum shown* limits how many fibres each layer draws (200 at first): the fibres with the largest absolute value are kept. *Add fibre file…* loads a `.mat` or `.trk` fibre file as one more layer. *Show fibres* in the stimulation editor adds a stimulation's connected fibres, and *Show in 3D* of a fibre-filtering result adds its fibres, coloured from white to red (positive) or blue (negative).
- *Maps*: one row per map (for example a sweet-spot or network-mapping result) with a tick box, a mode list and a *Threshold* slider. *Isosurface* draws smoothed red surfaces around the positive values and blue ones around the negative values above the threshold; *Slices* colours those values on the visible slices instead (only the first map in this mode is drawn). The threshold starts at a quarter of the largest absolute value.
- *View*: camera angles and *Reset view* (`R`).
- *Atlas*: *Show atlas overlay*, *Opacity*, the *Atlas set* (DISTAL Minimal by default; AHEAD is also included) and *Preset*. `Up` / `Down` step through the structures; `Space` shows or hides the selected one.

*Save scene* (`Ctrl+S`) saves the view as a PNG or JPEG image. *Open Viewer in separate window* (`F11`), in the same row as *Save scene* and *Help*, moves the viewer to its own window, for example on a second screen. There the same row holds *Full screen* and *Dock to main window*.

To use other Lead-DBS atlases, set the *Lead-DBS atlases directory* on the *Write electrode image* card to the `atlases` folder of a Lead-DBS installation.

## 8. Stimulation and the volume of activated tissue

The volume of activated tissue (VTA) is the region around the active contacts in which a stimulation setting is expected to activate axons. Wyss DBS Localizer computes the electric field around the lead and the VTA from it, and reports how much of each atlas structure the VTA covers.

### Entering the settings

On the *Volume of activated tissue* card, click *Edit stimulation…*. The subject needs a reconstruction.

1. **Label.** Each set of settings has a label, for example a visit or a programming session. *New…* creates one (letters, digits, `.`, `_`, `-`); *Duplicate* and *Delete* work on the selected label.
2. **Model.** *SimBio/FieldTrip* (an electric-field model, the default) or one of the simpler sphere models *Kuncel 2008*, *Maedler 2012* and *Dembek 2017*. *Fastfield* and *OSS-DBS* results can only be imported.
3. **Lead and source.** Choose *Right* or *Left*. Each lead has up to four sources. For each source set *Mode* (*Voltage (V)* or *Current (mA)*), *Amplitude* (0 switches the source off) and *Pulse width (µs)*.
4. **Contacts.** In the table, set each contact's *Polarity* (*Off*, *−* cathode, *+* anode) and, in current mode, its share in *Percent*. *Monopolar on contact N* is a quick way to set the common case. The *Case* row is the stimulator housing.
5. *Model settings* (folded away) holds the activation threshold, tissue conductivity and other parameters; *Restore defaults* resets them.

Tick *Preview* to see the VTA update in the 3D viewer while you edit. The first preview of a lead takes longer (up to tens of seconds) because the field of each contact is computed once and then reused.

*Save* stores the settings. *Compute VTA* saves and runs the *Volume of activated tissue* step; the result appears in the 3D viewer (*Show VTA*).

The editor belongs to the subject shown in its title (*Stimulation · sub-<id>*). *Save* and *Compute VTA* always act on that subject. When you select another subject in the subject list, the editor switches to it at once, unless:

- **you have unsaved settings.** The editor stays on its subject and a note names both subjects. Choose *Save and switch to sub-<id>*, *Discard and switch to sub-<id>* or *Stay on sub-<id>*. Nothing is thrown away or saved for the other subject until you choose.
- **a computation of the editor's subject is still running**, or a run is in progress. The editor stays on its subject and switches when it has finished.

A subject without saved settings opens with a new, empty label; as long as you do not change it, it does not count as unsaved and is not saved. If the newly selected subject cannot be shown, for example because it has no reconstruction yet, the editor says so for that subject and tries again when a step finishes or you press `F5`. With no subject selected it says that no subject is selected. Ticking subjects for a run of several subjects does not change the editor; only the highlighted subject counts. *Refine localization* follows the selection in the same way (section 6). While the editor shows another subject than the selected one, *Compute VTA* is greyed out. It is also greyed out while a run is in progress or something it must wait for is still busy; hover over it to see why.

*Connectivity* (collapsed under the editor) works on the saved label after its VTA is computed. Choose an installed *Connectome* (*Connectomes…* opens the download window), then *Compute connectivity*: a structural connectome writes the connectivity map and the connected-fibre file, a functional one writes the correlation maps, into the label's folder; the line below says *Computed with <connectome>* once they exist. *Show fibres* adds the connected fibres to the 3D viewer, coloured by strength. *Cancel* stops the job and writes nothing.

The group's own settings, opened from *Edit* in the group analysis, show the label fixed with a *Group stimulation* tag and have no *Compute VTA*: *Compute VTAs* in the group panel computes them.

Settings made in Lead-DBS (`stimparameters.mat`) are listed as *(Lead-DBS only: import it first)*; import them with `dbsloc stim import`.

### Results

The step writes, per label, under `stimulations/native/<label>/` and `stimulations/MNI152NLin2009bAsym/<label>/`: the VTA of each lead (`…_sim-binary_model-<model>_hemi-R.nii` / `hemi-L`), the electric field for the field model, Lead-DBS compatible `.mat` files, a summary `sub-<id>_desc-vta.json` and the atlas overlap `sub-<id>_desc-vtastats.json`.

### On the command line

```bash
dbsloc stim list my-study 01                              # labels of the subject
dbsloc stim show my-study 01 <label>                      # the settings of one label
dbsloc stim set my-study 01 <label> --from settings.json  # store settings from a file
dbsloc stim import my-study 01 stimparameters.mat --label <label>
dbsloc run my-study 01 --stages vat --vat-labels <label>  # compute the VTA
```

## 9. Importing Medtronic Percept session reports

The session report PDF that a Medtronic Percept PC or RC programmer saves contains the stimulation settings, impedances and events. Wyss DBS Localizer reads it and turns its stimulation groups into stimulation settings, so you do not have to type them in.

Choose *Import… → Session report (PDF)…* in the header (a subject must be open), or *Import from session report…* on the *Volume of activated tissue* card. You can select several PDFs. The *Import session report* window shows, for each file, the session date, the device, the leads and the groups.

- *Anonymise* removes the name, patient id, notes and device serial number, and moves every date by the same random number of days (at most 30) for this patient. The time of day, all stimulation values, impedances and events are kept.
- *Create stimulation settings from* chooses the settings at the start (*Initial*) or end (*Final*) of the session, *Both*, or *None*.
- *Groups* chooses which stimulation groups become stimulation labels; *Label prefix* is put in front of their names, for example the visit.
- *Keep a copy of the PDF in sourcedata* stores the original (not possible when anonymising).

The PDFs are read on your computer; nothing is sent anywhere. Imported reports are listed under *Reports* on the *Volume of activated tissue* card.

**The anonymisation key.** The date shift comes from a secret key that is created on first use and kept in your user folder (`%APPDATA%\dbsloc\anonymization.key` on Windows, `~/.local/share/dbsloc/anonymization.key` on Linux, `~/Library/Application Support/dbsloc/anonymization.key` on macOS). With the same key, all reports of one patient get the same shift, so intervals between visits stay correct. Back up this file; with a new key, later imports of the same patient get a different shift. The key and the shift are never written into the dataset.

So far only Medtronic Percept PC and RC reports are supported, and they have been tested with English reports only.

```bash
dbsloc report inspect report.pdf                                    # show what would be read, write nothing
dbsloc report import my-study 01 report.pdf --anonymize --snapshot final
dbsloc report list my-study 01
```

## 10. Several subjects at once

In the *Subjects* tab, tick the subjects to run (*Select all*, *Select none*, or the *Filter subjects* box). The run button then reads *Run (N subjects)*; the subjects run one after another, each through all selected steps.

*Review registrations* decides when you check the registrations:

- *During the run*: each subject waits for your review, as for a single subject.
- *After all subjects*: no subject waits. A subject whose registration needs a review stops there, is listed as *awaiting review*, and the next subject starts. When all subjects are through, the reviews open one after another; approving one runs that subject's remaining steps.
- *Off*: no review.

A table lists each subject's *Result* (*done*, *failed*, *awaiting review*, …), its steps and the time. If one subject fails, the others still run (on the command line, `--stop-on-first-failure` ends the batch instead). Stopping a batch asks whether to *Stop after this subject* or *Stop now*.

On the command line: `dbsloc run my-study 01 02 03`, `dbsloc run my-study --all-subjects` or `dbsloc run my-study --subjects-file ids.txt`. With `--defer-review`, a subject whose registration needs a review stops there and the next one starts; review those subjects afterwards in the application and run their remaining steps.

## 11. Group analysis

A group combines patients of one dataset for comparisons: which atlas structures their VTAs cover, where good and poor responders were stimulated, and how this relates to clinical scores. The group file can also be opened in Lead-DBS's Lead Group.

### Creating a group

Click *Group analysis* (`Ctrl+7`). Under *New group*, choose the *Dataset folder*, an optional *Title* and an *Identifier* (letters and digits), and click *Create group*. Groups are stored in `derivatives/leadgroup/<identifier>/` as a JSON record and a Lead Group `.mat` file next to it. *Open group…* opens an existing one.

### Setting it up

- *Patients*: *Add ticked subjects (N)* adds every subject ticked in the *Subjects* panel (or *Add sub-…* for the highlighted one when none is ticked); subjects already in the group are skipped. *Add patients* adds subjects by id. Untick *Use* to leave a patient out without removing it. *Group* puts the patient into group 1, 2, …. *Edit* opens the stimulation the patient uses in the group in the stimulation editor: the one named under *Settings from*, else the group's own settings (disabled without a reconstruction, while a job runs, or when the patient's dataset is not the current input directory); after you save, *Status* updates. The *Status* column says what each patient needs: *No reconstruction*, *No stimulation*, *Ready* or *Computed*.
- *Stimulation*: *Settings from* names a stimulation of the patients (for example the one you entered in the single-patient editor); it is copied into the group and kept in step with it. With *Group settings only* the group's own settings are used. *Model* and *Atlas set* apply to all patients.
- *Groups*: give each group a *Label* and a colour.
- *Clinical variables*: *Import table…* reads a TSV or CSV file with one row per patient. The first column (or one named `participant_id`, `subject` or `sub`) holds the subject id, with or without `sub-`; every other column is a variable. Empty cells, `NA`, `NaN` and `n/a` count as missing. *Colour by* colours the leads in 3D by a variable.
- *Display*: which contacts to show, and *Mirror sides* to show all leads on one side.

### Analyses

*Analyses* (below *Clinical variables*) explains a clinical variable with the stimulation. Choose *Sweet spot*, *Fibre filtering* or *Network mapping* and a *Variable* (needs an imported clinical table); optional *Covariates* are regressed out of the outcome first. *Run* is enabled when at least three included patients have computed VTAs (*Compute VTAs*); the reason it is disabled is shown beside it.

- *Sweet spot*: *Preset* sets the test and thresholds of a published study; *Test* is the voxel statistic. The result is a map of the voxels where stimulation goes with a better (sweet spot) or worse (sour spot) outcome.
- *Fibre filtering*: choose an installed structural *Connectome* and a *Test*; *Display percent* keeps the fibres whose score is within that percentage of the best score. The result is a set of fibres, coloured by score.
- *Network mapping*: choose an installed *Connectome* (structural or functional) and a *Map* and *Correlation*. *Run* first computes the missing or out-of-date connectivity map of every patient (progress shows *Connectivity maps*, patient by patient; a map is out of date when the stimulation image or the connectome changed), then the group map.
- *Cross-validation* judges how well the result predicts the outcome of patients it was not fitted on: *Leave one out* (default), *k-fold*, *Leave one cohort out*, or *None, permutation test*.
- *Advanced* holds every other setting with its default.

*Run* works on a worker with a progress bar and *Cancel*; the group is saved first and the finished analysis is listed under the kind, newest first, with its id, date, variable, number of patients *n*, correlation *r*, *p* and, for a two-valued outcome, *AUC*. A summary line goes to the *Output* panel. *Show in 3D* draws the result in the 3D viewer (fibres coloured by score, maps as smoothed surfaces), *Open folder* shows it in the file manager, *Delete* (after a confirmation) removes the result folder and its entry in the group. The command-line equivalents are `dbsloc group sweetspot`, `fibrefilter` and `netmap`.

### Results

- *Compute VTAs* saves the group, then for every ready patient computes the VTA, the atlas overlap statistics and the contact distances (and the mirrored volumes with *Mirror sides*). A line above the button sums this up; *Cancel* stops it. On the command line: `dbsloc group calc --copy-settings-from LABEL --sync-settings`.
- *Show in 3D viewer* shows all patients' leads together, coloured by group or by a clinical variable.
- *Export tables* writes the overlap tables (TSV and CSV) and the clinical table (TSV) to the group's `exports/` folder.
- *Save* writes the group file and the Lead Group file.

Further analyses run from the command line: `dbsloc group correlate` (correlation of overlaps with a clinical variable, two-group tests); sweet-spot mapping, fibre filtering and network mapping also run from the *Analyses* section (command line: `dbsloc group sweetspot`, `fibrefilter`, `netmap`). Fibre filtering and network mapping need a connectome. `dbsloc group --help` lists all group commands.

## 12. Connectomes

Fibre filtering and network mapping need a normative connectome: a model of the brain's fibre tracts (structural) or of correlated activity (functional), built from many healthy people or patients. Wyss DBS Localizer can download the connectomes that Lead-DBS offers.

Click *Connectomes…* in the header. The window has the list of datasets on the left, grouped as *Installed*, *Available* and *Other folders* (a coloured dot shows the state), with the filters *All*, *Installed*, *Structural* and *Functional* above it. Click a dataset to see its details on the right: where it comes from, its facts and, in folding sections, *Licence and terms*, *Required acknowledgement* and *How to get this dataset*. Tick *I have read and accept these terms*, then *Download*. Downloads are checked and can be resumed (*Resume download*). A connectome that is already installed (for example one imported from Lead-DBS) but has no licence record, or whose terms changed, shows *Licence and terms* open with the same tick boxes; tick them and click *Record acceptance* to save your acceptance (the button stays disabled until the required boxes are ticked). Nothing is downloaded or sent.

- Some datasets are freely available (for example *Global Tracking 19*, 3 GB).
- Some need you to accept the terms of the original study (the HCP-based *dTOR-985* sets, *GSP 1000*).
- Some can only be imported (*MGH-USC HCP 32*, the *PPMI* sets): Wyss DBS Localizer never downloads them, because each user needs their own approved data use agreement with the data provider. These datasets show *Import only: needs your own data access* and a numbered list under *How to get this dataset* with the application and download pages as links. After you have downloaded the archive in your web browser, tick both boxes and choose *Import archive…* (or *Choose folder…* for a folder that Lead-DBS already installed). If the archive keeps its published file name, its size and checksums are checked before it is extracted; an archive with another name is imported without that check, and the result says so.

Connectomes are large (from under 1 GB to about 220 GB). They are stored in `connectomes/` in your user data folder (`%APPDATA%\dbsloc`, `~/.local/share/dbsloc` or `~/Library/Application Support/dbsloc`); *Change…* at the top of the window picks another place, for example a large data drive. If you already have Lead-DBS connectomes, see *Using data from a Lead-DBS installation* below, or set *Lead-DBS connectomes folder* (second row at the top of the window, with *Change…* and *Clear*) to the folder that holds them; the folder you choose there wins over the environment variable `DBSLOC_LEADDBS_CONNECTOMES_DIR`, and without a choice the window shows the variable's value marked as coming from the environment variable. The *Import* menu (*Folder…*, *Archive…*) and the buttons *Choose folder…* and *Import archive…* of a dataset register a copy you already have.

**Faster loading with a ready-to-use copy.** Reading a large structural connectome takes several seconds each time (about 3 s for Gibbs 19 Full). The first time a job uses a structural connectome, a window *Speed up <name>?* offers to keep a ready-to-use copy on this computer: it needs about 14 bytes of disk space per fibre point (about 1 GB for Gibbs 19 Full) and later jobs start in a fraction of a second. Choose *Keep a copy* or *Not now*; the tick box *Don't ask again for this connectome* is ticked by default. *Keep a copy* is disabled when less than 5 GB would remain free. The results do not depend on the copy. In the *Connectomes…* window, the *Cache* menu has *Manage cache…*, which lists every copy with its size and last use, with *Select all*, *Select none* and *Delete selected…*, and lists the connectomes you declined with *Ask again*. The same *Cache* menu chooses *Ask* (default), *Always* or *Never*. The copies are stored in the local cache folder of your computer (`%LOCALAPPDATA%\dbsloc\cache`, `~/.cache/dbsloc` or `~/Library/Caches/dbsloc`), not in the roaming profile, and can be deleted at any time.

On the command line: `dbsloc connectome cache list`, `build <id>` and `clear <id>` (or `clear --all`), `dbsloc connectome list`, `dbsloc connectome licence <id>` and `dbsloc connectome fetch <id> --accept-terms` for the datasets that can be downloaded. For an import-only dataset, `fetch` prints the numbered steps and exits with code 2; after you have the archive, run `dbsloc connectome import <archive> --accept-terms --attest-data-use-agreement`.

### Using data from a Lead-DBS installation

If you already work with Lead-DBS, you do not have to download its atlases and connectomes again. The first time the application starts it asks: *Do you already use Lead-DBS on this computer?* Choose *Choose Lead-DBS folder…* and pick the Lead-DBS installation folder (the one that contains `templates`), the folder that contains `connectomes`, or a connectomes folder itself; or choose *Skip*. The question is asked only once. Nothing is ever written, moved or deleted in the Lead-DBS folder.

The window looks at the folder and lists what it found: *Atlases* and *Connectomes*, each with its size. A note says *Already included* (the atlas ships with the application), *Already there*, *Needs your data use agreement* or *Cohort data terms*. Then you choose:

- *Use them where they are*: no extra disk space. The two folders are stored as the settings *Lead-DBS atlases directory* and *Lead-DBS connectomes folder* and read in place. If you move or remove Lead-DBS, the atlases and connectomes are no longer available here.
- *Copy them into Wyss DBS Localizer*: each row gets a tick box (atlases that ship with the application and connectomes that are already there start unticked), the window shows the disk space needed and what is free, and *Import* is disabled with the reason when the space is too short. The copy shows a progress bar and can be cancelled; items already copied stay. Copied atlases go to the `atlases/leaddbs` folder in your user data folder and the *Lead-DBS atlases directory* is set to it; copied connectomes go to the Connectomes directory.

Connectomes that need a data use agreement or cohort terms are usable at once, but no licence record is written; the last step lists them and *Open Connectomes…* opens the window where you tick the boxes to record your acceptance. The Lead-DBS atlases come with the request of their owners not to use them commercially.

To do this later, use *Connectomes…*, then *Import*, then *From a Lead-DBS installation…*, or the button *Import from Lead-DBS…* under *Lead-DBS atlases directory* on the *Write electrode image* card.

Without a Lead-DBS installation, *Download atlases…* under *Lead-DBS atlases directory* lists the 73 atlases that Lead-DBS offers. Choose *Download* on a row to get that atlas from the Lead-DBS server; only that atlas is downloaded (from 0.4 MB to 812 MB), one at a time, and every file is checked against a known checksum. Downloaded atlases appear in the 3D viewer's *Atlas set* list on the subject's next *Reconstruct* or *Visualize* run, and *Remove* deletes one again. The atlases come with the request of their owners not to use them commercially; cite the publication the viewer shows for each atlas. On the command line: `dbsloc atlas list`, `dbsloc atlas fetch "<name>"` and `dbsloc atlas remove "<name>"`.

On the command line, `dbsloc leaddbs scan <folder>` lists what a folder holds (add `--json` for a machine-readable list) and ends with the two environment variables to set to use the data in place; `dbsloc leaddbs copy <folder>` copies the default selection (`--all`, `--atlas NAME` and `--connectome NAME` choose items, `--root DIR` sets the Connectomes directory).

## 13. Templates and backdrop images

The MNI152NLin2009bAsym template at 1 mm is included. Other MNI 2009 templates (2009b at 0.5 mm, 2009a and 2009c, asymmetric and symmetric) can be downloaded: choose one in the *Template* list on the *Normalize to template* card and click *Download*, or run `dbsloc template list` and `dbsloc template fetch <id>`. *Custom file…* uses any T1-weighted template on disk.

For the 3D viewer, two high-resolution images of a brain scanned after death at 7 tesla (Edlow et al. 2019) can be downloaded as backdrops: the whole brain at 0.5 mm (74 MB) and the deep brain region at 0.2 mm (1.15 GB). Choose them in the viewer's *Backdrop* list, or use `dbsloc backdrop list` and `dbsloc backdrop fetch <id>`.

Downloads go to your user data folder (see [Connectomes](#12-connectomes)) and are checked against a known checksum.

## 14. Using the command line

Everything the application does can also be run from a terminal with `dbsloc`, for example on a server or in a script. The Windows installer puts `dbsloc` on the path of new terminal windows; on Linux it is installed as a command.

```bash
dbsloc --help                    # all commands
dbsloc run --help                # all options of one command
```

| Command | Purpose |
| --- | --- |
| `dbsloc import` | import NIfTI files and DICOM folders into a subject |
| `dbsloc subject show` | show the scans found for one subject |
| `dbsloc run` | run the pipeline for one or more subjects |
| `dbsloc reco show` / `correct` | show or correct the reconstructed leads |
| `dbsloc stim` | stimulation settings |
| `dbsloc report` | Medtronic Percept session reports |
| `dbsloc group` | group analyses |
| `dbsloc connectome`, `dbsloc template`, `dbsloc backdrop` | downloads |
| `dbsloc viz` | open the 3D scene or the registration review |
| `dbsloc lock` | show or set the approval of a step |
| `dbsloc gpu probe` | check the graphics card |
| `dbsloc tutor` | upload reconstructions to Lead-Tutor ([section 17](#17-checking-the-accuracy-yourself-with-lead-tutor)) |

### Running the pipeline

```bash
dbsloc run my-study 01                                   # all steps
dbsloc run my-study 01 --el-model "Medtronic 3389"       # name the electrode model
dbsloc run my-study 01 --stages normalize,reconstruct    # only these steps
dbsloc run my-study 01 --no-vat                          # all steps except one (also --no-orientation, …)
dbsloc run my-study 01 --plan                            # show the plan, run nothing
dbsloc run my-study 01 --reg-backend ants                # register on the processor
dbsloc run my-study 01 --template MNI152NLin2009bAsym_res-0.5mm
dbsloc run my-study 01 --output-dir /data/results        # write results elsewhere
```

The step names for `--stages` are, in order: `bids`, `preprocess`, `precoreg`, `coreg`, `normalize`, `brainshift`, `reconstruct`, `orientation`, `vat`, `visualize`. (`brainshift` is not implemented yet and is skipped.)

When a display is available, `dbsloc run` opens the registration review and the 3D scene like the application. On a machine without a display, use `--headless`; the review images are still written. A registration that is not yet approved then stops the run; review it later in the application and run the remaining steps there. `--no-checkreg` skips the review altogether.

**What `dbsloc run` prints.** Progress and messages go to the terminal (standard error). At the end it prints a summary in JSON (standard output) that lists each step with its result: `ok:…` (done), `skip:…` (skipped, with the reason), `plan:…` (not run) or `error: …`. Save it with `dbsloc run my-study 01 > summary.json`.

**Exit codes**, for scripts: `0` all steps succeeded; `1` a step failed; `2` wrong options, or the subject could not be found; `130` stopped with `Ctrl+C`.

### Environment variables

Most options can also be set with environment variables, for example `DBSLOC_REG_BACKEND`, `DBSLOC_OUTPUT_DIR` or `DBSLOC_HEADLESS=1`. An option on the command line wins over the variable. [`RUN_REFERENCE.md`](RUN_REFERENCE.md) lists them all. The desktop application ignores them.

## 15. Where results are stored

For each subject, results go to `derivatives/leaddbs/sub-<id>/` in the dataset (or to the *Output directory* you chose). The layout follows Lead-DBS and BIDS conventions.

| Folder | Contents |
| --- | --- |
| `preprocessing/anat/` | the bias-corrected anchor scan (`…_desc-preproc_T1w.nii.gz`) |
| `coregistration/anat/` | post-operative scans aligned to the anchor (`…_space-anchorNative_desc-preproc_CT.nii.gz`) |
| `coregistration/transformations/` | the transforms between post-op scan and anchor |
| `normalization/anat/` | scans warped to the template (`…_space-MNI152NLin2009bAsym_desc-preproc_…nii.gz`) |
| `normalization/transformations/` | the warps between subject and template space (`…_from-anchorNative_to-MNI152NLin2009bAsym_mode-image_xfm.h5` and the reverse) |
| `coregistration/checkreg/`, `normalization/checkreg/` | the review images (PNG) |
| `coregistration/log/`, `normalization/log/` | how each registration was done, and its review decision |
| `reconstruction/` | the reconstruction: `sub-<id>_desc-reconstruction.json` and the Lead-DBS compatible `sub-<id>_desc-reconstruction.mat` |
| `reconstruction/viz/` | the 3D scene (`scene_manifest.json`) |
| `stimulations/` | stimulation settings and VTA results, one folder per label ([section 8](#8-stimulation-and-the-volume-of-activated-tissue)) |
| `clinical/reports/` | imported session reports |
| `locks/` | the approval state of each step |
| `prefs/` | settings shared with Lead-DBS, such as the electrode model |

The reconstruction JSON holds, for each lead, the head, tail and contact positions in the subject's own space (`native`) and in template space, the electrode model, and a record of how it was made: software version, settings, and any manual corrections. The `.mat` file contains the same reconstruction in the structure Lead-DBS uses.

Group analyses are stored in `derivatives/leadgroup/<group>/`.

## 16. Running steps again

**In the application**, a ticked step whose results already exist is skipped, and its card says so. To compute it again, tick *Rerun all selected steps* (you are asked to confirm, because existing results are replaced). Re-running *Reconstruct* this way also discards manual corrections. A step whose registration you rejected shows a *Rerun step* button on its card.

**On the command line**, `dbsloc run` computes every selected step again, except steps that are **approved**. A step is approved when you corrected or accepted every lead (*Reconstruct*, *Orientation*), or when you approve it with `dbsloc lock approve my-study 01 <step>`. `--overwrite-approved` computes approved steps again as well. `dbsloc lock show my-study 01 <step>` shows the state of a step.

**Later steps.** When a step is computed again, the results of the steps after it may no longer fit. Their approvals and registration reviews are therefore withdrawn (the files stay), and the next run computes them again. For example, a new coregistration withdraws the approvals of normalization and reconstruction. The *Output* tab and the terminal show a line such as `coreg re-ran; approvals invalidated: normalize, reconstruct`.

Approving a registration in the review records your verdict in the step's log; it does not stop a later command-line run from computing the registration again. To keep a registration, run only the later steps (`--stages`), or approve the step with `dbsloc lock approve`.

## 17. Checking the accuracy yourself with Lead-Tutor

[Lead-Tutor](http://tutor.lead-dbs.org) is a service of the Lead-DBS team for learning and checking electrode reconstructions. It holds expert reconstructions of 10 patients whose scans are public. You upload your reconstructions, and its feedback page shows how far each lead is from the expert's; the expert positions themselves stay hidden. Anyone can use it, so you can check the accuracy reported for Wyss DBS Localizer yourself:

1. **Download the data.** Get `LeadTutor.zip` (about 900 MB) from the OSF project [jxecg](https://osf.io/jxecg/) and extract it. The folder that contains `dataset_description.json` is the dataset; it already records the electrode model of each patient.
2. **Run the pipeline** for all 10 patients, without stopping for reviews:

   ```bash
   dbsloc run LeadTutor --all-subjects --headless --no-checkreg
   ```

   With a graphics card this takes roughly 10 to 15 minutes in total.
3. **Your Lead-Tutor user id.** Lead-Tutor files uploads under a user id, so you can compare your runs. The first upload asks you for one (letters, digits, `.`, `_` and `-`, for example `jdoe-dbsloc-2026`); press Enter to have a random id created instead. The id is saved and used for every later upload. `dbsloc tutor user-id` shows it, and `dbsloc tutor user-id --set <id>` changes it. Pick an id nobody else uses.
4. **Check, then upload:**

   ```bash
   dbsloc tutor submit-all-recon LeadTutor --require-all --dry-run   # show what would be sent
   dbsloc tutor submit-all-recon LeadTutor --require-all
   ```

   Only the template-space (MNI) coordinates of the two lead ends and of a point marking the orientation, for the 10 tutorial patients, are sent; no images, names or other data. The upload goes to port 8000 of `tutor.lead-dbs.org`; some institutional networks block that port.
5. **Read the result.** The feedback page (`http://tutor.lead-dbs.org/?userid=<your-id>`) opens in the browser. It shows, for each patient and lead, how far your lead is from the expert's. The service measures two distances per lead: at the deepest contact (head) and at the top of the contact array (tail). The accuracy in the README is, for each lead, the average of these two distances, averaged over all 20 leads.

The team's own upload of the version described in the README scored 1.15 mm on this measure. Small differences between computers and graphics cards are expected.

## 18. Troubleshooting

**Windows warns before the installer runs ("Windows protected your PC").** The program is not yet code-signed. Choose *More info*, then *Run anyway*.

**"No usable graphics driver found" or "Registration runs on the processor".** The graphics card could not be used, so the application draws its windows and runs the registration on the processor. Results are the same, but it is slower. Install the current driver from the graphics card maker; the notice has a button that helps you find it. `dbsloc gpu probe` shows which graphics card is found and whether it is used (`verdict: GO` means it is).

**The registration does not fit into the graphics card's memory.** With *Auto*, the step continues on the processor and says so. With *GPU (wgpu)*, choose a coarser *Resolution* on the *Normalize to template* card (or `--norm-isotropic-mm 1.0`), or use *ANTs (CPU)*.

**"no pre-op anchor" (`E_NO_ANCHOR`).** The subject has no pre-operative MRI in `ses-preop/anat`. Import one.

**"post-op modality … was requested but … has no post-op … image" (`E_MISS_POSTOP`).** *Post-op images to use* asks for a CT or MRI that the subject does not have. Set it to *Automatic*.

**"duplicate index rows" (`E_DUPL_RAW`).** Two entries in the subject's list of scans describe the same kind of scan. Import again with *Replace existing images*, or remove the duplicate entry from `rawdata/sub-<id>/rawimages.json`.

**"expected …/rawdata/sub-<id>" (`E_NO_SUBJ`) or "not a valid sub-<id> folder name".** The subject does not exist in this dataset, or its id contains characters other than letters and digits.

**"no electrode model set".** Choose the *Electrode model* on the *Reconstruct electrode* card, or pass `--el-model "<name>"`. A name that is not in the list gives "is not in the electrode catalogue"; the list in the application shows the exact spelling of every model.

**A lead was not found.** If only one lead could be traced, the step still succeeds, warns that the other lead "was not reconstructed and is left out of the file", and the reconstruction contains one lead. Check the coregistration first (a misaligned CT is the most common cause), then place the missing lead in *Refine localization*. For a post-operative MRI, try *Metal appearance: Bright metal artefact*.

**A lead is in the wrong place or tilted strangely.** Look at the coregistration review: the CT's skull must line up with the MRI. If it does not, reject it and run *Coregister* again, for example with the other engine. Otherwise correct the lead in *Refine localization*.

**"qc not approved".** The run stopped at a registration that has not been approved (for example in a run with `--headless`). Approve it in the *Registration review* tab or from the step's card, then run the remaining steps.

**DICOM import fails (`E_DCM2NIIX`).** The converter could not read the folder. Make sure the folder contains the DICOM files themselves (not a ZIP archive), and try one series folder at a time.

**The application closed unexpectedly.** On the next start it says so and offers *Open log folder*. Please send us the log, after checking it for patient information.

## 19. Getting help

Open an issue on the [issue tracker](https://github.com/WyssCenter/dbs-localizer/issues), or write to the maintainer, Richard Köhler (<richard.koehler@wysscenter.ch>). Please include the version (`dbsloc --version`), your operating system, the output of `dbsloc gpu probe`, and the session log of the application. The log is in `%APPDATA%\dbsloc\logs` on Windows, `~/.config/dbsloc/logs` on Linux and `~/Library/Application Support/dbsloc/logs` on macOS; `F1` shows the exact file and has an *Open log folder* button. Check that nothing you send contains patient information.
