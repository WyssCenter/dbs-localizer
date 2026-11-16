# Outputs and file layout

## Results of a subject

For each subject, results go to `derivatives/leaddbs/sub-<id>/` in the dataset (or to the *Output
directory* you chose; command line: `--output-dir`). The layout follows Lead-DBS and BIDS
conventions.

| Folder | Contents |
| --- | --- |
| `preprocessing/anat/` | the bias-corrected anchor scan (`…_desc-preproc_T1w.nii.gz`) |
| `coregistration/anat/` | post-operative scans aligned to the anchor (`…_space-anchorNative_desc-preproc_CT.nii.gz`) |
| `coregistration/transformations/` | the transforms between post-op scan and anchor |
| `normalization/anat/` | scans warped to the template (`…_space-MNI152NLin2009bAsym_desc-preproc_…nii.gz`) |
| `normalization/transformations/` | the warps between subject and template space (`…_from-anchorNative_to-MNI152NLin2009bAsym_mode-image_xfm.h5` and the reverse) |
| `coregistration/checkreg/`, `normalization/checkreg/` | the review images (PNG) |
| `coregistration/log/`, `normalization/log/` | how each registration was done, and its review decision |
| `reconstruction/` | the reconstruction: `sub-<id>_desc-reconstruction.json` and the Lead-DBS compatible `sub-<id>_desc-reconstruction.mat` |
| `reconstruction/viz/` | the 3D scene (`scene_manifest.json`) |
| `stimulations/` | stimulation settings and VTA results, one folder per label ([Stimulation](guide/stimulation.md#results)) |
| `clinical/reports/` | imported session reports |
| `log/` | the methods text `sub-<id>_desc-methods.txt`: one dated paragraph, with references, for each step that ran and each saved manual correction. New paragraphs are added at the end and nothing is overwritten, so you can copy the text into the methods section of a paper (`dbsloc methods my-study 01 --last` prints the latest paragraph of each step) |
| `locks/` | the approval state of each step |
| `prefs/` | settings shared with Lead-DBS, such as the electrode model |

The reconstruction JSON holds, for each lead, the head, tail and contact positions in the
subject's own space (`native`) and in template space, the electrode model, and a record of how it
was made: software version, settings, and any manual corrections. The `.mat` file contains the
same reconstruction in the structure Lead-DBS uses.

Group analyses are stored in `derivatives/leadgroup/<group>/`.

The scans themselves stay in `rawdata/sub-<id>/`; the import can keep a copy of the source DICOM
files in `sourcedata/sub-<id>/DICOM/` ([Importing scans](guide/import.md)).

The derivative names are specified in [`specs/01_bids.md`](../specs/01_bids.md) §7a, the
reconstruction file in [`specs/05_reconstruction.md`](../specs/05_reconstruction.md) §8 and the
`.mat` file in
[`specs/12_reconstruction_mat_and_manual.md`](../specs/12_reconstruction_mat_and_manual.md).

## Files outside the dataset

Settings, downloads and logs are kept in your user folders.

| What | Windows | Linux | macOS |
| --- | --- | --- | --- |
| User data: downloaded templates, backdrops and atlases, connectomes, the anonymisation key, the Lead-Tutor user id | `%APPDATA%\dbsloc` | `~/.local/share/dbsloc` | `~/Library/Application Support/dbsloc` |
| Session logs of the application | `%APPDATA%\dbsloc\logs` | `~/.config/dbsloc/logs` | `~/Library/Application Support/dbsloc/logs` |
| Cache: ready-to-use connectome copies, atlas surface meshes | `%LOCALAPPDATA%\dbsloc\cache` | `~/.cache/dbsloc` | `~/Library/Caches/dbsloc` |

The environment variables `DBSLOC_DATA_DIR` (user data), `DBSLOC_LOG_DIR` (session logs) and
`DBSLOC_CACHE_DIR` (the connectome copies; the atlas meshes stay in the default cache folder)
choose other folders, and the *Connectomes…* window can put
the connectomes elsewhere ([Connectomes](guide/connectomes.md#where-connectomes-are-stored)). The
connectome copies can be deleted at any time.
