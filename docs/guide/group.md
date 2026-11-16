# Group analysis

A group combines patients of one dataset for comparisons: which atlas structures their VTAs cover,
where good and poor responders were stimulated, and how this relates to clinical scores. The group
file can also be opened in Lead-DBS's Lead Group.

## Creating a group

Click *Group analysis* (`Ctrl+7`). Under *New group*, choose the *Dataset folder*, an optional
*Title* and an *Identifier* (letters and digits), and click *Create group*. Groups are stored in
`derivatives/leadgroup/<identifier>/` as a JSON record and a Lead Group `.mat` file next to it.
*Open group…* opens an existing one.

## Setting it up

- *Patients*: *Add ticked subjects (N)* adds every subject ticked in the *Subjects* panel (or
  *Add sub-…* for the highlighted one when none is ticked); subjects already in the group are
  skipped. *Add patients* adds subjects by id. Untick *Use* to leave a patient out without
  removing it. *Group* puts the patient into group 1, 2, …. *Edit* opens the stimulation the
  patient uses in the group in the stimulation editor: the one named under *Settings from*, else
  the group's own settings (disabled without a reconstruction, while a job runs, or when the
  patient's dataset is not the current input directory); after you save, *Status* updates. The
  *Status* column says what each patient needs: *No reconstruction*, *No stimulation*, *Ready* or
  *Computed*.
- *Stimulation*: *Settings from* names a stimulation of the patients (for example the one you
  entered in the single-patient editor); it is copied into the group and kept in step with it.
  With *Group settings only* the group's own settings are used. *Model* and *Atlas set* apply to
  all patients.
- *Groups*: give each group a *Label* and a colour.
- *Clinical variables*: *Import table…* reads a TSV or CSV file with one row per patient. The
  first column (or one named `participant_id`, `subject` or `sub`) holds the subject id, with or
  without `sub-`; every other column is a variable. Empty cells, `NA`, `NaN` and `n/a` count as
  missing. *Colour by* colours the leads in 3D by a variable.
- *Display*: which contacts to show, and *Mirror sides* to show all leads on one side.

## Analyses

*Analyses* (below *Clinical variables*) explains a clinical variable with the stimulation. Choose
*Sweet spot*, *Fibre filtering* or *Network mapping* and a *Variable* (needs an imported clinical
table); optional *Covariates* are regressed out of the outcome first. *Run* is enabled when at
least three included patients have computed VTAs (*Compute VTAs*); the reason it is disabled is
shown beside it.

- *Sweet spot*: *Preset* sets the test and thresholds of a published study; *Test* is the voxel
  statistic. The result is a map of the voxels where stimulation goes with a better (sweet spot)
  or worse (sour spot) outcome.
- *Fibre filtering*: choose an installed structural *Connectome* and a *Test*; *Display percent*
  keeps the fibres whose score is within that percentage of the best score. The result is a set of
  fibres, coloured by score.
- *Network mapping*: choose an installed *Connectome* (structural or functional) and a *Map* and
  *Correlation*. *Run* first computes the missing or out-of-date connectivity map of every patient
  (progress shows *Connectivity maps*, patient by patient; a map is out of date when the
  stimulation image or the connectome changed), then the group map.
- *Cross-validation* judges how well the result predicts the outcome of patients it was not fitted
  on: *Leave one out* (default), *k-fold*, *Leave one cohort out*, or *None, permutation test*.
- *Advanced* holds every other setting with its default.

*Run* works on a worker with a progress bar and *Cancel*; the group is saved first and the
finished analysis is listed under the kind, newest first, with its id, date, variable, number of
patients *n*, correlation *r*, *p* and, for a two-valued outcome, *AUC*. A summary line goes to
the *Output* panel. *Show in 3D* draws the result in the 3D viewer (fibres coloured by score, maps
as smoothed surfaces), *Open folder* shows it in the file manager, *Delete* (after a confirmation)
removes the result folder and its entry in the group. The command-line equivalents are
`dbsloc group sweetspot`, `fibrefilter` and `netmap`.

Fibre filtering and network mapping need a [connectome](connectomes.md).

## Results

- *Compute VTAs* saves the group, then for every ready patient computes the VTA, the atlas overlap
  statistics and the contact distances (and the mirrored volumes with *Mirror sides*). A line
  above the button sums this up; *Cancel* stops it. On the command line:
  `dbsloc group calc --copy-settings-from LABEL --sync-settings`.
- *Show in 3D viewer* shows all patients' leads together, coloured by group or by a clinical
  variable.
- *Export tables* writes the overlap tables (TSV and CSV) and the clinical table (TSV) to the
  group's `exports/` folder.
- *Save* writes the group file and the Lead Group file.

## On the command line

`dbsloc group correlate` correlates an atlas overlap measure with a clinical variable (Pearson and
Spearman, with a permutation p) and runs two-group tests; sweet-spot mapping, fibre filtering and
network mapping also run from the *Analyses* section (command line: `dbsloc group sweetspot`,
`fibrefilter`, `netmap`). `dbsloc group --help` lists all group commands; the
[command line reference](../cli.md#dbsloc-group) lists them with a short description.

Mirroring a lead to the other side uses a non-linear left-right flip transform of the template,
which is built on the graphics card the first time it is needed (minutes) and then kept
(`dbsloc group fliplr build` builds it ahead of time).

The group record, the Lead Group file and the analyses are specified in
[`specs/14_lead_group.md`](../../specs/14_lead_group.md).
