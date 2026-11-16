# Glossary

The words this documentation uses. Labels in *italics* are those of the application.

```{glossary}
Anchor
  The first pre-operative scan of a subject, usually the T1-weighted MRI. Every other scan of the
  subject is aligned to it. See [Basic ideas](guide/concepts.md).

Approval
  The state of a step that you have checked. An approved step is kept by later command-line runs
  instead of being computed again. It is stored in `locks/<step>.json`. See
  [Running steps again](guide/rerun.md).

Atlas set
  A collection of brain structures shown in the 3D viewer and used for the overlap statistics of a
  VTA, for example DISTAL Minimal or AHEAD. See
  [Atlases, templates and backdrops](guide/atlases-templates.md).

Backdrop
  The image shown on the slices of the 3D viewer: the template, the subject's normalized scans or a
  downloaded high-resolution image.

BIDS
  The Brain Imaging Data Structure, the folder layout for neuroimaging data that Lead-DBS and Wyss
  DBS Localizer use. Scans live under `rawdata/`, computed results under `derivatives/`.

Brain-shift correction
  A correction for the movement of the brain between the pre-operative and post-operative scans
  (step `brainshift`). Not implemented yet; the step is skipped.

Coregistration
  Aligning the scans of one subject with each other, here every scan with the {term}`anchor`, with
  a rigid transform (step `coreg`).

Connectome
  A normative model of the brain's fibre tracts (structural) or of correlated activity
  (functional), built from many people. Needed for fibre filtering and network mapping. See
  [Connectomes](guide/connectomes.md).

Electric field
  The field that a stimulation setting creates around the lead, computed from the contacts, the
  amplitudes and the tissue conductivity. The {term}`VTA` is derived from it.

Electrode model
  The type of lead that was implanted, for example *Medtronic 3389*. 73 models are in the
  catalogue; the contacts are placed from the model's geometry.

Engine
  What runs the registration steps: the graphics card (*GPU (wgpu)*) or the bundled ANTs programs
  on the processor (*ANTs (CPU)*); *Auto* chooses. See [Engine](guide/pipeline.md#engine).

Fibre filtering
  A group analysis that finds the fibres of a structural {term}`connectome` whose activation goes
  with a better or worse clinical outcome.

Head and tail
  The two markers of a lead: the head is the centre of the deepest contact level, the tail the
  4th contact level from the bottom (the top level on leads with fewer levels).

Label
  The name of one set of stimulation settings of a subject, for example a visit or a programming
  session.

Lead Group
  The group analysis of Lead-DBS. Wyss DBS Localizer writes its group file in the Lead Group
  format. See [Group analysis](guide/group.md).

Lead-DBS
  The MATLAB toolbox for localizing and visualizing DBS electrodes (Horn et al. 2019) whose
  workflow and file formats Wyss DBS Localizer follows.

Lead-Tutor
  A service of the Lead-DBS team with expert reconstructions of 10 public patients, used to check
  reconstructions. See [Accuracy and Lead-Tutor](guide/lead-tutor.md).

Methods text
  The dated paragraphs, with references, that each step adds to `log/sub-<id>_desc-methods.txt`.
  See [Methods text](guide/methods-text.md).

MNI space
  The space of the MNI152 brain templates. The default template is MNI152NLin2009bAsym.

N4
  N4 bias-field correction (Tustison et al. 2010), which removes slow brightness changes across a
  scan (step `preprocess`).

Native space
  The space of the subject's own scans after coregistration to the {term}`anchor`
  (`space-anchorNative` in file names).

Network mapping
  A group analysis that relates the connectivity maps of the patients' stimulations to a clinical
  variable.

Normalization
  Warping the subject's anatomy non-linearly into {term}`template space` (step `normalize`).

PaCER
  A method that reconstructs DBS leads from a post-operative CT alone (Husch et al. 2017). An
  optional component under its own licence.

Roll
  The rotation of a directional lead about its own axis, estimated by the orientation step or set
  by hand.

Scene
  The file `reconstruction/viz/scene_manifest.json` that the 3D viewer opens: template, leads,
  atlases and backdrops of a subject.

Subject
  One patient of a dataset, stored as `sub-<id>`.

Sweet spot
  A group analysis that maps the voxels where stimulation goes with a better (sweet spot) or worse
  (sour spot) clinical outcome.

SyN
  Symmetric normalization, the non-linear registration method of ANTs (Avants et al. 2008)
  used by the normalization.

Template space
  The space of the normalization template; results there can be compared across patients and with
  atlases.

TRAC/CORE
  The reconstruction method that traces the lead's centreline through axial slices (TRAC) and
  locates the lead's metal along that line to place the markers from the electrode model (CORE).

VTA
  Volume of activated tissue: the region around the active contacts in which a stimulation setting
  is expected to activate axons. See [Stimulation](guide/stimulation.md).
```
