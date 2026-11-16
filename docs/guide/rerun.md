# Running steps again

**In the application**, a ticked step whose results already exist is skipped, and its card says
so. To compute it again, tick *Rerun all selected steps* (you are asked to confirm, because
existing results are replaced). Re-running *Reconstruct* this way also discards manual
corrections. A step whose registration you rejected shows a *Rerun step* button on its card.

**On the command line**, `dbsloc run` computes every selected step again, except steps that are
**approved**. A step is approved when you corrected or accepted every lead (*Reconstruct*,
*Orientation*), or when you approve it with `dbsloc lock approve my-study 01 <step>`.
`--overwrite-approved` computes approved steps again as well. `dbsloc lock show my-study 01 <step>`
shows the state of a step.

**Later steps.** When a step is computed again, the results of the steps after it may no longer
fit. Their approvals and registration reviews are therefore withdrawn (the files stay), and the
next run computes them again. For example, a new coregistration withdraws the approvals of
normalization and reconstruction. The *Output* tab and the terminal show a line such as
`coreg re-ran; approvals invalidated: normalize, reconstruct`.

Approving a registration in the review records your verdict in the step's log; it does not stop a
later command-line run from computing the registration again. To keep a registration, run only the
later steps (`--stages`), or approve the step with `dbsloc lock approve`.

| When this step is computed again | the approvals of these steps are withdrawn |
| --- | --- |
| `preprocess` | `precoreg`, `coreg`, `normalize` and everything after them |
| `precoreg` | `coreg`, `normalize` and everything after them |
| `coreg` | `normalize`, `brainshift`, `reconstruct` and everything after them |
| `normalize` | `brainshift`, `reconstruct` and everything after them |
| `reconstruct` | `orientation`, `vat`, `visualize` |
| `orientation` | `vat`, `visualize` |
| `vat` | `visualize` |

With `--fingerprint-locks`, an approved step is skipped only while its input files keep the
content recorded when it was approved. The approval files are `locks/<step>.json` in the subject's
results folder ([Outputs](../outputs.md)). The rules are specified in
[`specs/00_overview.md`](../../specs/00_overview.md) §6.
