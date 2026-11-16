# Command line

Everything the application does can also be run from a terminal with `dbsloc`, for example on a
server or in a script. The Windows installer puts `dbsloc` on the path of new terminal windows; on
Linux and macOS it is installed as a command.

```bash
dbsloc --help                    # all commands
dbsloc run --help                # all options of one command
dbsloc --version                 # the version, and the git hash when the build knows it
```

| Command | Purpose |
| --- | --- |
| `dbsloc import` | import NIfTI files and DICOM folders into a subject |
| `dbsloc subject show` | show the scans found for one subject |
| `dbsloc run` | run the pipeline for one or more subjects |
| `dbsloc reco show` / `correct` | show or correct the reconstructed leads |
| `dbsloc methods` | print the subject's methods text for a paper; `--last` shows each step once, as it was last run |
| `dbsloc stim` | stimulation settings |
| `dbsloc report` | Medtronic Percept session reports |
| `dbsloc group` | group analyses |
| `dbsloc connectome`, `dbsloc template`, `dbsloc backdrop`, `dbsloc atlas` | downloads |
| `dbsloc leaddbs` | atlases and connectomes of an existing Lead-DBS installation |
| `dbsloc viz` | open the 3D scene or the registration review |
| `dbsloc lock` | show or set the approval of a step |
| `dbsloc gpu probe` | check the graphics card |
| `dbsloc tutor` | upload reconstructions to Lead-Tutor ([Accuracy and Lead-Tutor](guide/lead-tutor.md)) |

## Running the pipeline

```bash
dbsloc run my-study 01                                   # all steps
dbsloc run my-study 01 --el-model "Medtronic 3389"       # name the electrode model
dbsloc run my-study 01 --stages normalize,reconstruct    # only these steps
dbsloc run my-study 01 --no-vat                          # all steps except one (also --no-orientation, …)
dbsloc run my-study 01 --plan                            # show the plan, run nothing
dbsloc run my-study 01 --reg-backend ants                # register on the processor
dbsloc run my-study 01 --template MNI152NLin2009bAsym_res-0.5mm
dbsloc run my-study 01 --output-dir /data/results        # write results elsewhere
```

The step names for `--stages` are, in order: `bids`, `preprocess`, `precoreg`, `coreg`,
`normalize`, `brainshift`, `reconstruct`, `orientation`, `vat`, `visualize`. (`brainshift` is not
implemented yet and is skipped.) [The processing steps](guide/steps.md) describes each.

When a display is available, `dbsloc run` opens the registration review and the 3D scene like the
application. On a machine without a display, use `--headless`; the review images are still
written. A registration that is not yet approved then stops the run; review it later in the
application and run the remaining steps there. `--no-checkreg` skips the review altogether.

**What `dbsloc run` prints.** Progress and messages go to the terminal (standard error). At the end
it prints a summary in JSON (standard output) that lists each step with its result: `ok:…` (done),
`skip:…` (skipped, with the reason), `plan:…` (not run) or `error: …`. Save it with
`dbsloc run my-study 01 > summary.json`.

**Exit codes**, for scripts: `0` all steps succeeded; `1` a step failed; `2` wrong options, or the
subject could not be found; `130` stopped with `Ctrl+C`. With several subjects, see
[Several subjects at once](guide/batch.md#on-the-command-line).

### Options of `dbsloc run`

`dbsloc run <dataset_root> [<subject>…] [options]`. The subject ids may be given with or without
the `sub-` prefix. The options, grouped as `dbsloc run --help` groups them:

| Group | Options |
| --- | --- |
| Batch | `--all-subjects`, `--subjects-file <txt>`, `--continue-on-error` (the default), `--stop-on-first-failure`, `--defer-review` (implies `--headless`) |
| Stages | `--stages <names>`, `--no-normalize`, `--no-coreg` (drops `precoreg` and `coreg`), `--no-brainshift`, `--no-reconstruct`, `--no-orientation`, `--no-vat`, `--no-visualize`, `--no-index` (find the scans by scanning the `anat/` folders, ignoring `rawimages.json`) |
| Reconstruction | `--el-model <name>`, `--recon-method auto\|trac_core\|trac_core_native\|pacer`, `--recon-bright-metal`, `--postop-modality auto\|ct\|mri` |
| Execution | `--plan`, `--headless`, `--no-checkreg`, `--no-scene3d` |
| Locks | `--overwrite-approved`, `--fingerprint-locks` |
| Normalization | `--template <path or id>`, `--template-t2 <path>`, `--no-norm-multispectral`, `--norm-isotropic-mm <mm>\|full\|template`, `--norm-method ants_syn`, `--reg-backend auto\|wgpu\|ants`, `--preprocess-backend`, `--coregister-backend`, `--normalize-backend` |
| Output | `--log default\|quiet\|plain`, `--profile` (adds timings to the JSON summary), `--output-dir <directory>`, `--leaddbs-atlases-dir <directory>` |
| Stimulation | `--vat-backend auto\|wgpu\|cpu`, `--vat-labels <label>,…`, `--vat-stats-atlas <atlas set>` |

Any other option of the form `--ants-<flag> [value]` is passed to the ANTs programs, for example
`--ants-verbose 1`; `--ants-max-threads N` only limits the number of processor threads they use.
`dbsloc run --help` explains every option, and [Run reference](RUN_REFERENCE.md) gives the
details.

## Environment variables

Most options can also be set with environment variables, for example `DBSLOC_REG_BACKEND`,
`DBSLOC_OUTPUT_DIR` or `DBSLOC_HEADLESS=1`. An option on the command line wins over the variable.
[Run reference](RUN_REFERENCE.md) lists them all. The desktop application ignores them.

## All commands

The subcommands of each command, as `dbsloc <command> --help` lists them. Every subcommand has
its own `--help`.

### `dbsloc import`

Import NIfTI files and DICOM folders into a subject of a BIDS dataset
([Importing scans](guide/import.md)).

### `dbsloc subject`

- `show`: find a subject's scans in the dataset and print the paths as JSON.

### `dbsloc run`

Run the pipeline ([above](#running-the-pipeline)).

### `dbsloc reco`

- `show`: print the markers, roll, correction state and edit space of each lead as JSON.
- `correct`: move one lead (head and tail, optional roll) and rewrite the reconstruction
  ([Checking and correcting the electrodes](guide/correction.md#on-the-command-line)).

### `dbsloc methods`

Print the methods text of a subject ([Methods text](guide/methods-text.md)).

### `dbsloc stim`

- `list`: the stimulations of a subject, and Lead-DBS stimulation folders that can be imported.
- `show`: print one stimulation.
- `set`: check a stimulation JSON file and store it as the settings of a label.
- `import`: import a Lead-DBS `stimparameters.mat` as the settings of a label.

See [Stimulation and the volume of activated tissue](guide/stimulation.md).

### `dbsloc report`

- `inspect`: read a session report PDF and print what it holds, without writing anything.
- `import`: import session report PDFs into a subject: record, copy of the PDF, stimulation labels.
- `list`: the session reports imported into a subject.
- `anonymize`: write the anonymised record of a PDF to a file outside any dataset.

See [Session reports](guide/session-reports.md).

### `dbsloc group`

- `create`, `add`, `remove`, `set-group`: create a group, add or remove patients, put patients
  into a group number with a name and colour.
- `clinical import`, `clinical export`: clinical variables from or to a TSV or CSV table.
- `info`, `export`, `import-lead-group`: show a group, write its Lead Group file and clinical
  table, or create a group from a Lead Group file.
- `calc`: the group's VTAs, their atlas overlap and the contact distances of every patient.
- `stats`: atlas overlap tables and summaries.
- `correlate`: Pearson and Spearman correlations (with a permutation p) and two-group tests of an
  overlap measure with a clinical variable.
- `sweetspot`, `fibrefilter`, `netmap`: sweet-spot mapping, fibre filtering and network mapping,
  with cross-validation.
- `view`: write the scene of the group 3D view and print its path.
- `fliplr build`, `fliplr show`: the left-right flip transform used for mirroring.

See [Group analysis](guide/group.md).

### `dbsloc connectome`

- `list`, `info`, `licence`: the catalogue, what is known about one connectome, and its full terms.
- `fetch`: download a connectome after you accept its terms (checked, resumable).
- `import`, `forget`: register a connectome you obtained yourself, or stop listing it.
- `map`, `fibervals`: the structural or functional seed map of a stimulation, and the per-fibre
  values that fibre filtering uses.
- `cache list`, `cache build`, `cache clear`: the optional ready-to-use copy of a connectome.

See [Connectomes](guide/connectomes.md).

### `dbsloc template`, `dbsloc backdrop`, `dbsloc atlas`

- `template list`, `template fetch`: normalization templates.
- `backdrop list`, `backdrop fetch`: high-resolution backdrop images for the 3D viewer.
- `atlas list`, `atlas fetch`, `atlas remove`: the atlases that Lead-DBS offers.

Every download is checked against a known checksum. See
[Atlases, templates and backdrops](guide/atlases-templates.md).

### `dbsloc leaddbs`

- `scan`: list the atlases and connectomes of a Lead-DBS installation or data folder (writes
  nothing).
- `copy`: copy them into Wyss DBS Localizer (the Lead-DBS folder is never changed).

### `dbsloc viz`

- `scene`: open the interactive 3D scene of a scene file.
- `checkreg gen`, `checkreg review`: write one registration review image, or open the interactive
  review of the coregistration and normalization (needs a display).

### `dbsloc lock`

- `approve`: approve a step (`locks/<step>.json`).
- `show`: print the approval of one step as JSON.

See [Running steps again](guide/rerun.md).

### `dbsloc gpu`

- `probe`: create a graphics device, run a short self-test and print the graphics card, the
  graphics interface and the verdict; exit code 1 when no usable graphics card is found.

### `dbsloc tutor`

- `submit-recon`, `submit-all-recon`, `submit-file`: upload the electrode markers in MNI space to
  Lead-Tutor; the first two open its feedback page (`--no-open-browser` turns that off),
  `submit-file` only with `--open-browser`.
- `user-id`: show, set or forget the Lead-Tutor user id.

See [Accuracy and Lead-Tutor](guide/lead-tutor.md).
