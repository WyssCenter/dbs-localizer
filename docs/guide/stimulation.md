# Stimulation and the volume of activated tissue

The volume of activated tissue (VTA) is the region around the active contacts in which a
stimulation setting is expected to activate axons. Wyss DBS Localizer computes the electric field
around the lead and the VTA from it, and reports how much of each atlas structure the VTA covers.

## Entering the settings

On the *Volume of activated tissue* card, click *Edit stimulation…*. The subject needs a
reconstruction.

1. **Label.** Each set of settings has a label, for example a visit or a programming session.
   *New…* creates one (letters, digits, `.`, `_`, `-`); *Duplicate* and *Delete* work on the
   selected label.
2. **Model.** *SimBio/FieldTrip* (an electric-field model, the default) or one of the simpler
   sphere models *Kuncel 2008*, *Maedler 2012* and *Dembek 2017*. *Fastfield* and *OSS-DBS*
   results can only be imported.
3. **Lead and source.** Choose *Right* or *Left*. Each lead has up to four sources. For each source
   set *Mode* (*Voltage (V)* or *Current (mA)*), *Amplitude* (0 switches the source off) and *Pulse
   width (µs)*.
4. **Contacts.** In the table, set each contact's *Polarity* (*Off*, *−* cathode, *+* anode) and,
   in current mode, its share in *Percent*. *Monopolar on contact N* is a quick way to set the
   common case. The *Case* row is the stimulator housing.
5. *Model settings* (folded away) holds the activation threshold, tissue conductivity and other
   parameters; *Restore defaults* resets them.

Tick *Preview* to see the VTA update in the 3D viewer while you edit. The first preview of a lead
takes longer (up to tens of seconds) because the field of each contact is computed once and then
reused.

*Save* stores the settings. *Compute VTA* saves and runs the *Volume of activated tissue* step; the
result appears in the 3D viewer (*Show VTA*).

## Which subject the editor shows

The editor belongs to the subject shown in its title (*Stimulation · sub-&lt;id&gt;*). *Save* and
*Compute VTA* always act on that subject. When you select another subject in the subject list, the
editor switches to it at once, unless:

- **you have unsaved settings.** The editor stays on its subject and a note names both subjects.
  Choose *Save and switch to sub-&lt;id&gt;*, *Discard and switch to sub-&lt;id&gt;* or *Stay on
  sub-&lt;id&gt;*. Nothing is thrown away or saved for the other subject until you choose.
- **a computation of the editor's subject is still running**, or a run is in progress. The editor
  stays on its subject and switches when it has finished.

A subject without saved settings opens with a new, empty label; as long as you do not change it,
it does not count as unsaved and is not saved. If the newly selected subject cannot be shown, for
example because it has no reconstruction yet, the editor says so for that subject and tries again
when a step finishes or you press `F5`. With no subject selected it says that no subject is
selected. Ticking subjects for a run of several subjects does not change the editor; only the
highlighted subject counts. *Refine localization* follows the selection in the same way
([Checking and correcting the electrodes](correction.md)). While the editor shows another subject
than the selected one, *Compute VTA* is greyed out. It is also greyed out while a run is in
progress or something it must wait for is still busy; hover over it to see why.

## Connectivity

*Connectivity* (collapsed under the editor) works on the saved label after its VTA is computed.
Choose an installed *Connectome* (*Connectomes…* opens the download window, see
[Connectomes](connectomes.md)), then *Compute connectivity*: a structural connectome writes the
connectivity map and the connected-fibre file, a functional one writes the correlation maps, into
the label's folder; the line below says *Computed with &lt;connectome&gt;* once they exist. *Show
fibres* adds the connected fibres to the 3D viewer, coloured by strength. *Cancel* stops the job
and writes nothing.

## Group settings and Lead-DBS settings

The group's own settings, opened from *Edit* in the group analysis, show the label fixed with a
*Group stimulation* tag and have no *Compute VTA*: *Compute VTAs* in the group panel computes them
([Group analysis](group.md)).

Settings made in Lead-DBS (`stimparameters.mat`) are listed as *(Lead-DBS only: import it first)*;
import them with `dbsloc stim import`.

## Results

The step writes, per label, under `stimulations/native/<label>/` and
`stimulations/MNI152NLin2009bAsym/<label>/`: the VTA of each lead
(`…_sim-binary_model-<model>_hemi-R.nii` / `hemi-L`), the electric field for the field model,
Lead-DBS compatible `.mat` files, a summary `sub-<id>_desc-vta.json` and the atlas overlap
`sub-<id>_desc-vtastats.json`.

The electric field is computed on the graphics card when the problem fits into its memory, and on
the processor otherwise (`--vat-backend auto|wgpu|cpu` on the command line). The overlap
statistics use the DISTAL Minimal atlas unless `--vat-stats-atlas` names another atlas set.

## On the command line

```bash
dbsloc stim list my-study 01                              # labels of the subject
dbsloc stim show my-study 01 <label>                      # the settings of one label
dbsloc stim set my-study 01 <label> --from settings.json  # store settings from a file (UTF-8, or UTF-16 as a Windows redirect writes it)
dbsloc stim import my-study 01 stimparameters.mat --label <label>
dbsloc run my-study 01 --stages vat --vat-labels <label>  # compute the VTA
```

The settings format, the models and the outputs are specified in
[`specs/07_vat.md`](../../specs/07_vat.md); connectivity in
[`specs/15_connectomes.md`](../../specs/15_connectomes.md).
