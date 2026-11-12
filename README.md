# Wyss DBS Localizer

[![Check](https://github.com/WyssCenter/dbs-localizer/actions/workflows/check.yml/badge.svg)](https://github.com/WyssCenter/dbs-localizer/actions/workflows/check.yml)
[![Full](https://github.com/WyssCenter/dbs-localizer/actions/workflows/full.yml/badge.svg)](https://github.com/WyssCenter/dbs-localizer/actions/workflows/full.yml)

Wyss DBS Localizer finds the electrodes of a deep brain stimulation (DBS) implant in a patient's brain scans and shows where they sit relative to brain structures.

It starts from an MRI taken before surgery and a CT or MRI taken after surgery. The software aligns the scans, maps them to a standard brain template (MNI space), traces each electrode lead and its contacts, and shows the result in 3D next to an atlas of deep brain structures. From there it can model the tissue that a given stimulation setting activates and compare many patients in a group analysis.

## Why this software exists

Researchers who study DBS need to know where each contact of an implanted lead sits in the brain, both for the single patient and across groups of patients. The standard open tool for this, [Lead-DBS](https://www.lead-dbs.org), runs inside MATLAB, which needs a paid licence, and its registration steps can take tens of minutes per patient.

Wyss DBS Localizer does the same kind of work without MATLAB:

- **One installer, no extra software.** The desktop application and the command-line tool include everything they need: the registration tools (ANTs), the DICOM converter (dcm2niix), a brain template and an atlas.
- **Fast.** Registration runs on the graphics card (NVIDIA, AMD, Intel or Apple). On a laptop whose only graphics card is the one built into the processor (Intel Arc 140V, integrated in an Intel Core Ultra 200V laptop processor), a full run takes about one minute per patient (43–84 s on the 10 patients of the validation dataset). Without a usable graphics card, it falls back to the processor.
- **Compatible with Lead-DBS.** It reads the Lead-DBS folder layout (BIDS) and also writes its reconstruction, stimulation and group results in the Lead-DBS file formats, so existing datasets and analyses carry over.
- **Checked against experts.** On the 10 patients of the Lead-DBS tutorial dataset, the reconstructed leads lie on average 1.15 mm from the expert reconstructions (see [Accuracy](#accuracy)).

It is intended for **research** on DBS imaging: clinical neuroscientists, neurosurgery and neurology research groups, and anyone who already uses Lead-DBS and wants a faster, standalone tool. It is a new implementation written from written specifications ([`specs/`](specs)), not a translation of the Lead-DBS code.

## What it does

- **Import** NIfTI files or DICOM folders into a BIDS dataset, with suggestions for which scan is which.
- **Prepare and align** the scans: bias-field correction, alignment of the post-operative scan to the pre-operative MRI, and non-linear registration to the MNI152NLin2009bAsym template (T1w and T2w together when both exist).
- **Check registrations** on overlay slides inside the application, and approve or reject them.
- **Reconstruct electrodes** from post-operative CT (TRAC/CORE or PaCER) or MRI, for 73 electrode models, and detect the orientation of directional leads (Boston Scientific and Abbott; experimental).
- **Correct by hand** any lead that was traced wrongly, in the *Refine localization* view.
- **Model stimulation**: electric field and volume of activated tissue from stimulation settings, entered by hand or read from Medtronic Percept session reports (PDF).
- **View in 3D**: leads, contacts and atlas structures (DISTAL Minimal and AHEAD atlases included).
- **Analyse groups**: overlap with atlas structures, sweet-spot maps, fibre filtering, network mapping and correlations with clinical scores, using normative connectomes that it can download for you.

The project is young (version 0.1). Its outputs have not yet been compared number by number with Lead-DBS on every step; [`CHANGELOG.md`](CHANGELOG.md) lists what each release contains.

## Installation

### Requirements

| | |
| --- | --- |
| **Operating system** | Windows 10 or 11 (64-bit); Ubuntu 24.04 or newer, Debian 13 or newer, or another 64-bit (x86_64) Linux such as Arch or Fedora. macOS on Apple silicon works when built from source (see [CONTRIBUTING.md](CONTRIBUTING.md)). |
| **Graphics card** | Optional but recommended. Any graphics card with a current driver for Vulkan, DirectX 12 or Metal speeds up registration many times over. Without one, the application still runs: it draws its windows in software and registers on the processor. |
| **Other software** | None. Python, MATLAB and Rust are **not** needed to use the software. |

### Windows

1. Download `dbs-localizer-setup.exe` from the latest release on the [Releases page](https://github.com/WyssCenter/dbs-localizer/releases).
2. Run it. It installs for your user account only and does not need administrator rights.
3. Windows may show a SmartScreen warning because the program is not yet code-signed: choose *More info*, then *Run anyway*.

The installer adds *Wyss DBS Localizer* to the Start menu and the desktop, and makes the `dbsloc` command available in new terminal windows. Remove it again under *Settings → Apps*.

### Debian and Ubuntu

Download `dbs-localizer_<version>_amd64.deb` from the [Releases page](https://github.com/WyssCenter/dbs-localizer/releases) and install it:

```bash
sudo apt install ./dbs-localizer_<version>_amd64.deb
```

### Other Linux distributions

Download `dbsloc-linux.tar.gz` from the [Releases page](https://github.com/WyssCenter/dbs-localizer/releases), then:

```bash
tar -xzf dbsloc-linux.tar.gz
./dbsloc-linux/install.sh        # installs into ~/.local and adds a menu entry
./dbsloc-linux/install.sh -u     # removes it again
```

The desktop application needs the system packages `gtk3`, `vulkan-icd-loader`, `libxkbcommon` and `fontconfig` (Arch names); `install.sh` prints the install command for your distribution if one is missing.

> [`docs/RELEASING.md`](docs/RELEASING.md#installing-a-release) also describes installing from the command line.

### Check that it works

Open a new terminal and run:

```bash
dbsloc --version     # prints the version
dbsloc gpu probe     # shows which graphics card will be used, if any
```

## Getting started

### 1. Put your scans in a dataset

The software works on a folder in the [BIDS](https://bids.neuroimaging.io) layout, the same layout Lead-DBS uses:

```text
my-study/
└── rawdata/
    └── sub-01/
        ├── ses-preop/anat/
        │   ├── sub-01_ses-preop_acq-iso_T1w.nii.gz
        │   └── sub-01_ses-preop_acq-iso_T2w.nii.gz     (optional)
        └── ses-postop/anat/
            └── sub-01_ses-postop_CT.nii.gz
```

You do not have to create this by hand. In the application, choose *Import… → Images into a subject…* (`Ctrl+I`), add the DICOM folders or NIfTI files, check the suggested assignments and click *Import*. From the command line:

```bash
dbsloc import my-study --subject 01 --dicom path/to/dicom-folder --dry-run   # show what would be imported
dbsloc import my-study --subject 01 --dicom path/to/dicom-folder --auto      # import the suggested scans
```

`dbsloc import --help` explains how to assign a scan by hand when the suggestion is wrong.

Results are written to `my-study/derivatives/leaddbs/sub-01/`, next to the raw data, as in Lead-DBS.

### 2a. Use the desktop application

Start *Wyss DBS Localizer* from the Start menu or application menu (or run `dbs-localizer` in a terminal).

1. Choose the input directory (the BIDS dataset) with *Browse…* (`Ctrl+O`).
2. Select a patient in **Subjects**.
3. In **Pipeline**, tick the steps you want and choose the electrode model and template.
4. Click **Run pipeline** (`Ctrl+R`).

During the run the application pauses after each registration so you can check it in the *Registration review* tab. When the run is done, the **3D Viewer** shows the leads with the atlas. If a lead is in the wrong place, open *Refine localization* on the Reconstruct card and move it.

The start screen also offers a guided run that walks through these steps one at a time.

### 2b. Use the command line

```bash
dbsloc run my-study 01                                  # run all steps for subject 01
dbsloc run my-study 01 --el-model "Medtronic 3389"      # name the electrode model if Lead-DBS has not stored it
dbsloc run my-study 01 --plan                           # only show which steps would run
dbsloc run my-study --all-subjects --defer-review       # every subject, one after another
dbsloc reco show my-study 01                            # print the reconstructed lead positions
```

`dbsloc run` prints progress to the terminal and a summary in JSON at the end. Every command explains its options with `--help`, for example `dbsloc run --help`. `dbsloc --help` lists all commands, including stimulation settings (`stim`), group analysis (`group`), connectome downloads (`connectome`) and session-report import (`report`).

### Keyboard shortcuts

Press `F1` in the application for the complete list, including the shortcuts of the registration review and the *Refine localization* view.

| Keys | Action |
| --- | --- |
| `Ctrl+O` | Choose the dataset folder |
| `Ctrl+I` | Import images into a subject |
| `Ctrl+R` | Run the selected steps |
| `Ctrl+.` | Stop the run |
| `F5` | Scan the input directory again |
| `Alt+Up` / `Alt+Down` | Previous / next subject |
| `Ctrl+1` … `Ctrl+7` | Go to Subjects, Pipeline, Output, 3D Viewer, Registration review, Refine localization, Group analysis |
| `Ctrl+H` | Go to the start screen |
| `Ctrl+L` / `Ctrl+Shift+C` | Clear / copy the output log |
| `F1` | Show the help |

In the 3D Viewer:

| Keys or mouse | Action |
| --- | --- |
| Left drag | Rotate |
| Right drag, or `Ctrl` + left drag | Move the view |
| Mouse wheel, or middle drag | Zoom |
| Double click, or `R` | Reset the view |
| `1` / `2` / `3` | Show or hide the axial / coronal / sagittal slice |
| `Up` / `Down`, `Space` | Step through atlas structures, show or hide the selected one |
| `Ctrl+S` | Save the view as an image |
| `F11` | Open the viewer in its own window |

The mouse works as in Lead-DBS by default; the viewer's *Mouse & touchpad* section offers an alternative layout.

## Documentation

- **User guide:** [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) explains every part of the application and the command line, step by step, with a troubleshooting section.
- **In the application:** `F1` for shortcuts and system information; every control has a tooltip.
- **On the command line:** `dbsloc --help` and `dbsloc <command> --help`.
- **All run options and environment variables:** [`docs/RUN_REFERENCE.md`](docs/RUN_REFERENCE.md).
- **How each step works:** the specifications in [`specs/`](specs), starting with [`specs/00_overview.md`](specs/00_overview.md).
- **What changed in each release:** [`CHANGELOG.md`](CHANGELOG.md).

## Accuracy

The reference dataset is the Lead-DBS tutorial cohort ("Lead-Tutor", [OSF jxecg](https://osf.io/jxecg/)): 10 patients with expert reconstructions. With the default settings, the leads found by Wyss DBS Localizer lie on average **1.15 mm** from the expert reconstructions (distance at the two ends of the contact array, averaged over all 20 leads). You can check this yourself: download the tutorial data, run Wyss DBS Localizer on it and upload the result under your own user id to the Lead-DBS team's [Lead-Tutor](http://tutor.lead-dbs.org) service, which reports the distance of each lead from the expert reconstruction. The [user guide](docs/USER_GUIDE.md#17-checking-the-accuracy-yourself-with-lead-tutor) gives the commands. Details per registration step are in [`docs/gpu_syn_cohort_report.md`](docs/gpu_syn_cohort_report.md), [`docs/gpu_coreg_report.md`](docs/gpu_coreg_report.md) and [`docs/gpu_n4_report.md`](docs/gpu_n4_report.md).

## Getting help and reporting problems

- **Questions and bug reports:** open an issue on the [issue tracker](https://github.com/WyssCenter/dbs-localizer/issues). For questions you do not want to ask in public, write to the maintainer, Richard Köhler (<richard.koehler@wysscenter.ch>). Please include the version (`dbsloc --version`), your operating system and graphics card (`dbsloc gpu probe`), what you did and what happened.
- **Log files:** the application writes a log for every session, which helps us find the cause of a problem. You find it in `%APPDATA%\dbsloc\logs` on Windows, `~/.config/dbsloc/logs` on Linux and `~/Library/Application Support/dbsloc/logs` on macOS; `F1` shows the exact path. Check that a log contains no patient information before you attach it.
- **Contributing:** bug fixes, new features, documentation and test data are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) explains how to build the software from source, run the tests and propose a change. Everyone taking part in the project follows our [Code of Conduct](CODE_OF_CONDUCT.md).

## Citing

If you use Wyss DBS Localizer in your research, please cite the software (name, version from `dbsloc --version`, and this repository) and the methods it builds on:

- Horn A. et al. (2019). Lead-DBS v2: Towards a comprehensive pipeline for deep brain stimulation imaging. *NeuroImage* 184, 293–316.
- Husch A. et al. (2018). PaCER — A fully automated method for electrode trajectory and contact reconstruction in deep brain stimulation. *NeuroImage: Clinical* 17, 80–89 (when you use PaCER).
- Avants B. B. et al. (2008). Symmetric diffeomorphic image registration with cross-correlation. *Medical Image Analysis* 12(1), 26–41.

The atlases, templates and connectomes have their own citations: the application shows them where you choose an atlas, backdrop or connectome, and [`NOTICE`](NOTICE) lists those of the bundled data.

## License

Copyright © 2026 Wyss Center for Bio and Neuroengineering. Wyss DBS Localizer is free software under the GNU General Public License, version 3 only (`GPL-3.0-only`); see [`LICENSE`](LICENSE). It comes without any warranty. The Wyss Center name and logo are trademarks and are not covered by that licence. The registration engine (`fastwarp/crates/fastwarp-core`, `fastwarp-niftiio`, `fastwarp-matfile`, `fastwarp-reg`, `fastwarp-gpu`: the in-process replacement for ANTs registration and N4, on CPU and GPU) is licensed under the Apache License 2.0. Every source file names its licence in an SPDX header; [`docs/LICENSING.md`](docs/LICENSING.md) maps the licences to the directories.

The software includes third-party programs and data under their own licences: ANTs (Apache-2.0), dcm2niix (BSD), the MNI152 templates, and the DISTAL Minimal (GPL-3) and AHEAD atlases. [`NOTICE`](NOTICE) lists each with its licence.

## Acknowledgements

The workflow follows [Lead-DBS](https://www.lead-dbs.org) by Andreas Horn and colleagues. Registration follows [ANTs](https://github.com/ANTsX/ANTs); DICOM conversion uses [dcm2niix](https://github.com/rordenlab/dcm2niix). Wyss DBS Localizer is developed at the Wyss Center for Bio and Neuroengineering, Geneva.
