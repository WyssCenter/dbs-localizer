# Checking registrations

A wrong registration makes everything after it wrong, so the software asks you to look at each
one. With *Review registrations* set to its default, the run pauses after *Coregister* and after
*Normalize* and opens the *Registration review* tab (in the guided run, the review appears inside
the guide).

Each image shows the moving scan over the fixed one, for example the post-op CT over the pre-op
MRI, or the patient's MRI over the template. Check that edges, ventricles and the skull line up.

- Approve each image with `A` and reject it with `D`; both move on to the next image. On the last
  image the buttons read *Approve & continue run* and *Reject & stop run*.
- One rejected image stops the run and marks the step *needs rework*. Change the settings of that
  step (for example the engine, the template or the resolution) and click *Rerun step* on its
  card.
- Arrow keys move between images, `Page Up` / `Page Down` between slices; `1`, `2`, `3` show one
  plane, `0` all three; `R` resets the zoom. *Window / level* offers presets for soft tissue and
  bone.

You can review a finished registration later with *Review registration* on its card. The cards
show *Reviewed ✔*, *Rejected* or *Not reviewed*. Later steps warn when they use a registration
that is rejected or not reviewed, but they still run.

## On the command line

When a display is available, `dbsloc run` opens the same review after the coregistration and the
normalization. With `--headless` it opens no window but still writes the review images (PNG) to
`coregistration/checkreg/` and `normalization/checkreg/`; a registration that is not yet approved
then stops the run. `--no-checkreg` skips the review altogether. To continue a headless run,
review the registration in the application and run the remaining steps there, or approve the
step with
`dbsloc lock approve my-study 01 coreg` (or `normalize`) and run again
([Running steps again](rerun.md)).

How the review images are drawn and how a decision is stored is specified in
[`specs/03_coregistration.md`](../../specs/03_coregistration.md) §12 and
[`specs/08_addendum_interactive_volume_viewer.md`](../../specs/08_addendum_interactive_volume_viewer.md).
