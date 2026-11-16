# Basic ideas

**Dataset.** All scans of a study live in one folder in the [BIDS](https://bids.neuroimaging.io)
layout, the layout Lead-DBS also uses. Scans go under `rawdata/`; everything Wyss DBS Localizer
computes goes under `derivatives/leaddbs/`. You never have to edit `rawdata/` by hand: the
[import](import.md) does it for you.

**Subject.** One patient, stored as `sub-<id>`, for example `sub-01`. The id contains only letters
and digits.

**Pre-operative and post-operative scans.** The pre-operative MRI (T1-weighted, and T2-weighted if
you have one) shows the anatomy. The post-operative CT or MRI shows the implanted leads. The first
pre-operative scan, usually the T1w, is the **anchor**: every other scan of the subject is aligned
to it.

**Template space.** To compare patients and to use a brain atlas, the anchor is warped to a
standard brain, the MNI152NLin2009bAsym template ("MNI space"). The bundled template has 1 mm
voxels; others can be [downloaded](atlases-templates.md#templates).

**Steps.** The pipeline runs a fixed sequence of steps: preprocessing, alignment of the scans,
normalization to the template, electrode reconstruction, orientation of directional leads, the
volume of activated tissue, and the 3D scene. You choose which steps run. Each step uses the
results of the steps before it. [The processing steps](steps.md) describes each one.

**Approval.** When you have checked the leads (corrected or accepted each one), the reconstruction
is approved, and later command-line runs keep it instead of computing it again. Reviewing a
registration records your verdict, but does not protect the registration from being computed
again. See [Running steps again](rerun.md).

**Electrode model.** The reconstruction needs to know which lead was implanted (for example
*Medtronic 3389* or *Boston Scientific Vercise Directed*). 73 models are supported. If the subject
was set up in Lead-DBS before, the model stored there is used; otherwise you choose it.
