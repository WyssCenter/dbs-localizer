# Running the pipeline

Select a subject in *Subjects*. The *Pipeline* tab shows one card per step. Tick a card to include
the step; its *Options* then unfold. The arrow on a card folds it to one row; *Collapse all* /
*Expand all* folds every card. *Select all steps* and *Clear selection* tick or untick all of
them.

| Card | What the step does | Main options |
| --- | --- | --- |
| *Preprocess MRI / CT* | Corrects slow brightness changes across the anchor scan (N4 bias-field correction). Before *Coregister* or *Normalize*, the anchor is also roughly aligned with the template, automatically. | *Engine* |
| *Coregister post-operative image* | Aligns the post-operative CT or MRI to the pre-operative anchor. Several starting positions are tried, so large head tilts are handled. | *Engine*; *Post-op images to use* (*Automatic* takes the CT when both a CT and an MRI exist) |
| *Normalize to template* | Warps the anatomy non-linearly into template space (ANTs SyN). With a pre-op T2w, T1w and T2w are registered together, which is more accurate. | *Template*; *T2 template*; *Multispectral (T1w + T2w)*; *Resolution* (*Template resolution* is recommended); *Engine* |
| *Reconstruct electrode* | Finds each lead and its contacts in the post-operative scan and maps them to template space. | *Electrode model* (required); *Method* (*Automatic* is recommended); *Metal appearance* (for post-op MRI: try *Bright metal artefact* if the lead is not found) |
| *Estimate directional orientation* | Estimates the rotation of directional leads from the CT artefact of their orientation marker. Experimental; Boston Scientific and Abbott leads only. | |
| *Volume of activated tissue* | Computes the electric field and the VTA for the saved stimulation settings; skipped when there are none. See [Stimulation](stimulation.md). | *Edit stimulation…*; *Import session report…* (always visible on the card) |
| *Write electrode image* | Writes the 3D scene the viewer opens. | *Lead-DBS atlases directory* |

[The processing steps](steps.md) describes what each step does and writes in more detail.

## Engine

*Auto (GPU when available)* uses the graphics card when it passes a short self-test at start-up,
and the bundled ANTs on the processor otherwise. *GPU (wgpu)* insists on the graphics card; *ANTs
(CPU)* always uses the processor. Both reach the same accuracy on the validation data; the
graphics card is many times faster. The graphics-card engine is [FastWarp](https://fastwarp.readthedocs.io), which is also available as a standalone Python package and command line interface.

## Electrode model

Choose the implanted model in the list, or *From each subject's settings* to use the model stored
by Lead-DBS for each subject. Without a model, the reconstruction cannot run.

## Run options

Below the cards:

- *Number of threads*: how many processor cores the processor-based steps may use. Lower it if you
  need the computer for other work during a run.
- *Rerun all selected steps*: compute ticked steps again even if results exist. Without it, steps
  with existing results are skipped (see [Running steps again](rerun.md)).
- *Review registrations*: when to check registrations (see [Checking registrations](review.md)
  and [Several subjects at once](batch.md)).
- *Stop on first error*: skip the remaining steps of a subject once one of its steps fails.

## Running and its status

Each card shows whether it can run: ✔ ready, ⚠ a warning, ✖ something is missing (for example
*Needs a normalized post-operative image*). If a ticked step cannot run, the panel lists the
reasons under *Before this run can start*, and *Untick the steps that cannot run* removes them.

Click *▶ Run pipeline* (`Ctrl+R`). The cards show *queued*, *running*, *done*, *skipped* (with the
reason) or *failed*; the *Output* tab shows the details. *⏹ Stop* (`Ctrl+.`) stops the run. *Show
results* opens the subject's results in the file manager. *Methods text* opens the text for the
methods section of a paper ([Methods text](methods-text.md)).

A full run with the default settings takes about one minute per subject with a graphics card, and
considerably longer on the processor alone.

*Subject images* at the top of the panel lists the subject's scans; *View* opens any of them in a
small viewer, which is useful to check that the right scan was imported.

On the command line, `dbsloc run` runs the same steps; see [Command line](../cli.md#running-the-pipeline).
