# Checking and correcting the electrodes

After *Reconstruct*, the Reconstruct card shows one line per lead: whether it was found,
corrected or accepted. Always look at the result in the 3D viewer and in *Refine localization*.

## Refine localization

Click *Refine localization* on the Reconstruct card. The view shows the post-operative scan in
planes along the lead, with the lead model drawn on top.

- Select the deepest contact with `H` (*Head*) or the tail contact with `T` (*Tail*: the 4th
  contact level from the bottom, or the top level on leads with fewer levels), then drag it, or
  move it with the arrow keys (0.25 mm per press; with `Shift` 1 mm, with `Ctrl` 0.05 mm).
- *Keep lead length* (`K`) keeps the distance between head and tail fixed.
- For directional leads, set the *Roll* with the slider or `[` / `]`. *Flip 180°* and *Other
  candidate* switch between the orientations the automatic estimate found; *Automatic* returns to
  the estimate.
- *Accept side* (`Space`) marks a lead as checked without changing it.
- *Undo* / *Redo* (`Ctrl+Z` / `Ctrl+Y`) work per lead.
- *Save corrections* (`Ctrl+Enter`) writes them to the reconstruction; *Discard* (`Esc`) throws
  them away.

*Refine localization* follows the subject you select in the subject list. With unsaved
corrections it stays on its subject and asks: *Save and switch to sub-&lt;id&gt;*, *Discard and
switch to sub-&lt;id&gt;* or *Stay on sub-&lt;id&gt;*. While a correction is being saved, it switches
once the save has finished. A subject without a reconstruction is shown as such. Opening the lead
of the newly selected subject never holds back a run or an image import.

## What a correction changes

The contacts always follow the electrode model: you place the two ends, the software places the
contacts in between. A corrected lead shows *● Manually corrected* on the card. Once every lead is
corrected or accepted, the reconstruction counts as approved, and later runs keep it. Results that
depend on the leads, such as the volume of activated tissue and the 3D scene, must be computed
again; the message after saving says which. Saving a correction also adds a paragraph to the
[methods text](methods-text.md).

## On the command line

`dbsloc reco show my-study 01` prints the lead positions and `dbsloc reco correct` moves a lead
(`dbsloc reco correct --help`). For example:

```bash
dbsloc reco correct my-study 01 --side right --head X,Y,Z --tail X,Y,Z --roll DEG
dbsloc reco correct my-study 01 --accept left
```

`--side`, `--head` and `--tail` go together; `--roll` is optional; `--accept right|left` (which
can be repeated) marks a lead as checked without changing it. The coordinates are in the edit
space that `dbsloc reco show` prints: the subject's native space for a CT reconstruction, template
space for a post-operative MRI. A correction is refused (`E_RECON_manual`) when the lead was made
from other inputs than the current ones; run the reconstruction first.

A later `dbsloc run` keeps every corrected or accepted lead while its inputs are unchanged, and
re-traces only the others; `--overwrite-approved` re-traces every lead.
Specification: [`specs/05_reconstruction.md`](../../specs/05_reconstruction.md) §9a.
