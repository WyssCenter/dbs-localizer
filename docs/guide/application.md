# The application window

Start *DBS Localizer* from the Start menu or application menu, or run `dbs-localizer` in a
terminal.

## Start screen

The start screen lists your *Recent subjects* and *Recent folders* and offers *Open BIDS
folder…*, *Open BIDS subject…* and *Import raw NIfTI / DICOM…*. Tick *Guide me through the steps*
to go through a run one step at a time (see [Guided run](#guided-run)), or click *Skip* to go to the
full workspace.

## Tabs

The workspace has these tabs. You can drag them to arrange the window; *Settings → Reset panel
layout* restores the original arrangement.

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

*Registration review*, *Refine localization*, *Stimulation* and *Group analysis* appear beside the
*3D Viewer* when you need them and can be closed (the ✖ on the tab) while they hold nothing;
nothing of them is restored at the next start.

## Input and output directories

Above the tabs, two rows of the same shape pick the *Input directory* (the BIDS dataset, with
*Recent* and *Browse…*, `Ctrl+O`) and the *Output directory*. Both fields are editable. By default
the output directory is empty and its grey hint shows where results go (`derivatives/leaddbs`
inside the input directory); type or browse to put them somewhere else, for example to keep the
dataset read-only, and clear the field or choose *Default* in *Recent* to go back.

## Header

The header has *Home* (`Ctrl+H`, back to the start screen), *Import…*, *Connectomes…* and *Group
analysis* (`Ctrl+7`) on the left, and *Settings* (colour theme, *Reset panel layout*) and *Help*
(`F1`, shortcuts and system information) on the right. The subject list and the step outputs are
scanned again whenever the window regains focus, and on `F5`.

## Guided run

The guide walks through *Subject*, *Preprocess*, *Coregister*, *Normalize*, *Reconstruct*,
*Stimulation* and *Review*. On each step you can *Run this step*, *Use existing* results, *Skip*
it, or go *Back*. *Stimulation* is optional: with a saved stimulation it offers *Compute VTA* for
the one you choose (shown in the 3D viewer from *Review*), otherwise it reads *Skip*. *Switch to
full view* leaves the guide at any time.

## Keyboard shortcuts

```{include} ../../README.md
:start-after: <!-- docs:shortcuts -->
:end-before: <!-- /docs:shortcuts -->
```

## Session log

Every session of the application writes a log file: `%APPDATA%\dbsloc\logs` on Windows,
`~/.config/dbsloc/logs` on Linux and `~/Library/Application Support/dbsloc/logs` on macOS. `F1`
shows the current file. The 20 newest files are kept. If a session did not end normally, the next
start screen offers *Open log folder*.
