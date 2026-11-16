# Methods text

Every step that completes adds a dated paragraph, with references, to the subject's methods text
`log/sub-<id>_desc-methods.txt` in the subject's results folder. You can copy it into the methods
section of a paper.

- **What adds an entry.** Each completed step (`preprocess`, `precoreg`, `coreg`, `normalize`,
  `reconstruct`, `orientation`, `vat`), each VTA calculation of a group, and each saved manual
  correction that moved a marker or set a roll. Skipped steps, plan-only runs and failed steps add
  nothing, with one exception: when the `vat` step fails or is cancelled, the stimulation labels it
  had already computed are described.
- **Nothing is overwritten.** New paragraphs are added at the end; earlier ones, also those of
  repeated runs of a step, stay as they are. The file is the history of what was done to the
  subject.
- **What a paragraph says.** The images and modalities, the method, the engine (the graphics card
  or the ANTs programs, with the version the ANTs programs report), the template space, the
  electrode model and the reconstruction method, as far as the step's records state them; and the
  references of the methods that ran. It contains no subject id, no file path and no patient data.

Each entry starts with a line of the form `[<UTC time>] <step> | Wyss DBS Localizer <version>`.

## In the application

*Methods text* in the *Pipeline* tab opens the text: one paragraph for each step that ran for this
subject, with references and, where the ANTs programs were used, their version. *Latest run of
each step only* shows each step once, as it was last run; untick it to see every run, oldest first
(the choice is remembered). *Copy* copies the text shown, *Open folder* shows the file
(`log/sub-<id>_desc-methods.txt`) in the file manager, *Refresh* reads it again, and `Esc` closes
the window; the text also updates when a run ends and after you save a corrected lead. The button
is greyed out until a subject is selected and a step has run for it.

## On the command line

```bash
dbsloc methods my-study 01           # the whole text
dbsloc methods my-study 01 --last    # the latest entry of each step
dbsloc methods my-study 01 --path    # only the path of the file
```

Exit codes: 0 printed, 1 the subject has no methods text yet (or, with `--last`, no entry), 2
unknown subject or `--last` together with `--path`.

The methods text is specified in [`specs/00_overview.md`](../../specs/00_overview.md) §5.3.
