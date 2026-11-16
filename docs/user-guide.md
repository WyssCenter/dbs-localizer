# User guide

This guide explains how to use Wyss DBS Localizer, from your first import to group analyses. It is
written for researchers who use the software. You do not need to know how to program.

Installation is described in [Installation](installation.md). Words in *italics* are labels you
see on screen; `code` is something you type. The [glossary](glossary.md) explains the terms.

::::{grid} 1 2 2 2
:gutter: 3

:::{grid-item-card} Basic ideas
:link: guide/concepts
:link-type: doc

Dataset, subject, anchor, template space, steps, approval and electrode model.
:::

:::{grid-item-card} Importing scans
:link: guide/import
:link-type: doc

DICOM folders and NIfTI files into a BIDS dataset, in the application or with `dbsloc import`.
:::

:::{grid-item-card} The application window
:link: guide/application
:link-type: doc

Start screen, tabs, input and output directories, the guided run and keyboard shortcuts.
:::

:::{grid-item-card} Running the pipeline
:link: guide/pipeline
:link-type: doc

The step cards, the registration engine, run options and run status.
:::

:::{grid-item-card} The processing steps
:link: guide/steps
:link-type: doc

Preprocessing, coregistration, normalization, reconstruction and orientation: what each step
does and writes.
:::

:::{grid-item-card} Checking registrations
:link: guide/review
:link-type: doc

Approving or rejecting each registration in the *Registration review* tab.
:::

:::{grid-item-card} Checking and correcting the electrodes
:link: guide/correction
:link-type: doc

Moving a lead, setting the roll of a directional lead, and accepting a lead in *Refine
localization*.
:::

:::{grid-item-card} The 3D viewer
:link: guide/viewer
:link-type: doc

Mouse, slices, backdrops, atlases, fibre and map layers, saving the view.
:::

:::{grid-item-card} Stimulation and the volume of activated tissue
:link: guide/stimulation
:link-type: doc

Stimulation settings, the electric field, the VTA and connectivity of a stimulation.
:::

:::{grid-item-card} Session reports
:link: guide/session-reports
:link-type: doc

Stimulation settings from Medtronic Percept session reports, with anonymisation.
:::

:::{grid-item-card} Several subjects at once
:link: guide/batch
:link-type: doc

Running a batch and deciding when to review its registrations.
:::

:::{grid-item-card} Group analysis
:link: guide/group
:link-type: doc

Groups, clinical variables, atlas overlap, sweet spots, fibre filtering and network mapping.
:::

:::{grid-item-card} Connectomes
:link: guide/connectomes
:link-type: doc

Downloading or importing normative connectomes, their terms and the fast-loading cache.
:::

:::{grid-item-card} Atlases, templates and backdrops
:link: guide/atlases-templates
:link-type: doc

The bundled atlases and template, downloads, and data of an existing Lead-DBS installation.
:::

:::{grid-item-card} Running steps again
:link: guide/rerun
:link-type: doc

When steps are skipped, approved or computed again.
:::

:::{grid-item-card} Methods text
:link: guide/methods-text
:link-type: doc

The dated paragraphs for the methods section of a paper.
:::

:::{grid-item-card} Accuracy and Lead-Tutor
:link: guide/lead-tutor
:link-type: doc

The accuracy on the Lead-DBS tutorial cohort, and how to check it yourself.
:::
::::

```{toctree}
:hidden:

guide/concepts
guide/import
guide/application
guide/pipeline
guide/steps
guide/review
guide/correction
guide/viewer
guide/stimulation
guide/session-reports
guide/batch
guide/group
guide/connectomes
guide/atlases-templates
guide/rerun
guide/methods-text
guide/lead-tutor
```
