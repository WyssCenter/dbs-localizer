# Several subjects at once

In the *Subjects* tab, tick the subjects to run (*Select all*, *Select none*, or the *Filter
subjects* box). The run button then reads *Run (N subjects)*; the subjects run one after another,
each through all selected steps.

*Review registrations* decides when you check the registrations:

- *During the run*: each subject waits for your review, as for a single subject.
- *After all subjects*: no subject waits. A subject whose registration needs a review stops there,
  is listed as *awaiting review*, and the next subject starts. When all subjects are through, the
  reviews open one after another; approving one runs that subject's remaining steps.
- *Off*: no review.

A table lists each subject's *Result* (*done*, *failed*, *awaiting review*, …), its steps and the
time. If one subject fails, the others still run (on the command line, `--stop-on-first-failure`
ends the batch instead). Stopping a batch asks whether to *Stop after this subject* or *Stop now*.

## On the command line

`dbsloc run my-study 01 02 03`, `dbsloc run my-study --all-subjects` or
`dbsloc run my-study --subjects-file ids.txt` (one id per line; lines starting with `#` are
ignored). With `--defer-review`, a subject whose registration needs a review stops there and the
next one starts; review those subjects afterwards in the application and run their remaining
steps.

Each subject gets a header line, then its progress; at the end a table lists every subject with
its result and time. Exit codes: 0 when no subject failed (awaiting review counts as not failed),
1 when any subject failed or was not found, 2 when every subject is unknown or on a usage error,
130 on `Ctrl+C`.

Batch runs are specified in
[`specs/13_import_and_batch_reference.md`](../../specs/13_import_and_batch_reference.md) Part 2.
