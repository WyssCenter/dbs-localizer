# Atlases, templates and backdrops

## Bundled atlases

Two atlases ship with the application and are shown in the [3D viewer](viewer.md):

- **DISTAL Minimal** (Ewert et al. 2017), the default atlas of Lead-DBS, copied unchanged from the
  Lead-DBS data package with its colours, presets and citation. The atlas owners ask that the
  Lead-DBS atlases are not used commercially; this request travels with the data.
- **AHEAD** structure masks (Alkemade et al. 2020), under the Creative Commons Attribution 4.0
  licence.

The atlas overlap statistics of a stimulation use DISTAL Minimal unless another atlas set is
chosen. [Licences and notices](../licence.md) lists the citation and licence of each.

## Lead-DBS atlases

Without a Lead-DBS installation, *Download atlases…* under *Lead-DBS atlases directory* (on the
*Write electrode image* card) lists the 73 atlases that Lead-DBS offers. Choose *Download* on a
row to get that atlas from the Lead-DBS server; only that atlas is downloaded (from 0.4 MB to
812 MB), one at a time, and every file is checked against a known checksum. Downloaded atlases
appear in the 3D viewer's *Atlas set* list on the subject's next *Reconstruct* or *Write electrode
image* run, and *Remove* deletes one again. The atlases come with the request of their owners not
to use them commercially; cite the publication the viewer shows for each atlas. On the command
line: `dbsloc atlas list`, `dbsloc atlas fetch "<name>"` and `dbsloc atlas remove "<name>"`.

To use the atlases of a Lead-DBS installation, set the *Lead-DBS atlases directory* to its
`atlases` folder (command line: `--leaddbs-atlases-dir`, or the environment variable
`DBSLOC_LEADDBS_ATLASES_DIR`), or follow the next section.

## Data from a Lead-DBS installation

If you already work with Lead-DBS, you do not have to download its atlases and connectomes again.
The first time the application starts it asks: *Do you already use Lead-DBS on this computer?*
Choose *Choose Lead-DBS folder…* and pick the Lead-DBS installation folder (the one that contains
`templates`), the folder that contains `connectomes`, or a connectomes folder itself; or choose
*Skip*. The question is asked only once. Nothing is ever written, moved or deleted in the Lead-DBS
folder.

The window looks at the folder and lists what it found: *Atlases* and *Connectomes*, each with its
size. A note says *Already included* (the atlas ships with the application), *Already there*,
*Needs your data use agreement* or *Cohort data terms*. Then you choose:

- *Use them where they are*: no extra disk space. The two folders are stored as the settings
  *Lead-DBS atlases directory* and *Lead-DBS connectomes folder* and read in place. If you move or
  remove Lead-DBS, the atlases and connectomes are no longer available here.
- *Copy them into Wyss DBS Localizer*: each row gets a tick box (atlases that ship with the
  application and connectomes that are already there start unticked), the window shows the disk
  space needed and what is free, and *Import* is disabled with the reason when the space is too
  short. The copy shows a progress bar and can be cancelled; items already copied stay. Copied
  atlases go to the `atlases/leaddbs` folder in your user data folder and the *Lead-DBS atlases
  directory* is set to it; copied connectomes go to the Connectomes directory.

Connectomes that need a data use agreement or cohort terms are usable at once, but no licence
record is written; the last step lists them and *Open Connectomes…* opens the window where you
tick the boxes to record your acceptance ([Connectomes](connectomes.md)). The Lead-DBS atlases
come with the request of their owners not to use them commercially.

To do this later, use *Connectomes…*, then *Import*, then *From a Lead-DBS installation…*, or the
button *Import from Lead-DBS…* under *Lead-DBS atlases directory* on the *Write electrode image*
card.

On the command line, `dbsloc leaddbs scan <folder>` lists what a folder holds (add `--json` for a
machine-readable list) and ends with the two environment variables to set to use the data in
place; `dbsloc leaddbs copy <folder>` copies the default selection (`--all`, `--atlas NAME` and
`--connectome NAME` choose items, `--root DIR` sets the Connectomes directory).

## Templates

The MNI152NLin2009bAsym template at 1 mm is included. Other MNI 2009 templates (2009b at 0.5 mm,
2009a and 2009c, asymmetric and symmetric) can be downloaded: choose one in the *Template* list on
the *Normalize to template* card and click *Download*, or run `dbsloc template list` and
`dbsloc template fetch <id>`. *Custom file…* uses any T1-weighted template on disk.

## Backdrop images

For the 3D viewer, two high-resolution images of a brain scanned after death at 7 tesla (Edlow et
al. 2019) can be downloaded as backdrops: the whole brain at 0.5 mm (74 MB) and the deep brain
region at 0.2 mm (1.15 GB). Choose them in the viewer's *Backdrop* list, or use
`dbsloc backdrop list` and `dbsloc backdrop fetch <id>`.

Downloads go to your user data folder ([Where connectomes are
stored](connectomes.md#where-connectomes-are-stored)) and are checked against a known checksum.

Atlases are specified in [`specs/11_atlases.md`](../../specs/11_atlases.md), templates in
[`specs/04_normalization.md`](../../specs/04_normalization.md) §3, the import from a Lead-DBS
installation in [`specs/17_leaddbs_data_import.md`](../../specs/17_leaddbs_data_import.md).
