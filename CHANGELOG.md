# Changelog

All notable changes to Wyss DBS Localizer are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Data of an existing Lead-DBS installation can be used: at the first start the application asks for the Lead-DBS folder, lists the atlases and connectomes it finds with their sizes, and either uses them where they are or copies them. Also available later from the *Connectomes* window, and as `dbsloc leaddbs scan` / `dbsloc leaddbs copy`.
- Release packages contain `THIRD_PARTY_LICENSES.md` with the licences of the Rust libraries built into the programs, and the third-party licence list of ITK beside the ANTs programs.
- The 73 atlases that Lead-DBS offers can be downloaded one by one without a Lead-DBS installation: *Download atlases…* under *Lead-DBS atlases directory*, or `dbsloc atlas list` / `dbsloc atlas fetch` / `dbsloc atlas remove`. Only the chosen atlas is fetched from the Lead-DBS server, and every file is checked against a pinned SHA-256.
- Every connectome download (Gibbs 19, NKI-RS 169, Tor PD, Pediatric rs-fMRI, dTOR-985, GSP 1000) is checked against a pinned SHA-256; a connectome archive that you import and that is named like a catalogue file is checked against its pinned size and checksum.

### Changed

- The PaCER electrode reconstruction moved into its own component, `crates/dbsloc-pacer`, licensed under the GNU Affero General Public License, version 3 only (AGPL-3.0-only), like the original PaCER. It is included by default (cargo feature `pacer`), so the programs and release packages are now distributed as a combination of GPL-3.0-only and AGPL-3.0-only code. Building with `--no-default-features` gives a GPL-3.0-only program without PaCER, in which the PaCER method is refused with a message naming the missing feature and shown as unavailable in the application. Reconstruction results are unchanged.
- Connectomes that need your own data use agreement (PPMI 85, PPMI 74 Patients 15 Controls, MGH-USC HCP 32) are no longer downloaded by the application. Their entries show numbered steps for getting access and the archive; you then import the archive or folder.
- The dTOR-985 connectomes are downloaded from the authors' figshare records instead of lead-dbs.org.

- Wyss DBS Localizer is now free software under the GNU General Public License, version 3 (`GPL-3.0-only`); it was proprietary before. The registration engine (CPU and GPU replacement for ANTs registration and N4) stays under the Apache License 2.0. Every source file names its licence; the licence texts ship in `LICENSES/` beside `LICENSE` and `NOTICE`. The Wyss Center name and logo remain trademarks and are not covered by the licence.

### Fixed

- Group analyses: mirrored (left-right flipped) stimulation images made through the flip transform are now written on the mirrored grid of their source. Before, an image whose grid was not symmetric about the midline gave an empty or misplaced mirrored file. Mirrored files that are not on the mirrored grid of their source, as written by earlier versions for such images, are now refused by sweet-spot mapping and skipped with a note by fibre filtering and the group 3D view; mirrored files on the correct grid are still used. Group *Calculate* now deletes the mirrored files of a patient whenever it recomputes that patient's template-space stimulation images, also when mirroring is off or the run fails before mirroring, so outdated mirrored files are no longer left beside new images; a mirrored file that cannot be deleted, for example because another program holds it open, now fails that patient with `E_GROUP_FLIP` and names the file; a label folder that cannot be listed counts the same, and a run that was cancelled stays cancelled, with the same message. Run group *Calculate* with mirroring again for groups calculated before.
- A post-op row of a hand-written `rawimages.json` without a `modality` no longer counts as CT. It now follows the row's `suffix` (CT) and `acq` (ax, cor, sag); a row with neither is placed by its file name (CT, coronal or sagittal when the name says so, else axial MR). An approved coregistration of such a subject is not redone by itself: run it again (*Rerun all selected steps*, or `--overwrite-approved`).
- Fibre filtering with mirroring now uses the mirrored image of a patient who has a lead on one side only (it was left out with a wrong "no flipped image" note).
- A pipeline step that runs again (for example with another template, another registration engine or *Rerun all selected steps*) now withdraws its earlier approval before it starts, so a later run no longer skips it as approved while its new result has not been reviewed. A result approved in its review during the same run keeps the approval, and so does a step that stops before it replaces any result (for example when a tool or template is missing). When such a step fails or is cancelled after replacing results, the approvals of the steps that depend on it are withdrawn too.
- The message shown when a registration waits for review now also tells command-line users to run `dbsloc lock approve` (with `--output-dir` if the run used one) before the next `dbsloc run`.
- The *Stimulation* and *Refine localization* tabs now follow the subject selected in the subject list, as the 3D viewer does. Before, they kept showing the previous subject after another one was selected. With unsaved settings or corrections the tab stays on its subject, says that another subject is selected and offers *Save and switch*, *Discard and switch* or *Stay*; the empty label the editor creates for a subject without saved settings does not count as unsaved. A computation or save of the tab's own subject, or a run, finishes first. A selected subject that cannot be shown, for example one without a reconstruction, is named with the reason. The 3D viewer no longer draws the stimulation of the previous subject over the selected one.

### Security

- Updated bundled third-party components to versions without known security advisories (Python bindings, the browser client's TLS and root certificates, a yanked clipboard library).

## [0.1.0] - 2026-10-26

First release, for Windows and Linux (x86_64). The binaries are not code-signed, so Windows SmartScreen warns before the installer runs.

### Added

- Desktop application `dbs-localizer`: start screen with recent subjects and folders, guided run, dockable Subjects / Pipeline / Output / 3D Viewer panels, batch queue over several subjects, per-session log file; starts on a software renderer when no usable graphics driver is installed.
- Command-line tool `dbsloc` running the same pipeline: `run` (single subject or batch, `--plan`, JSON summary), `import`, `reco show` / `reco correct`, `stim`, `group`, `connectome`, `report`, `template`, `backdrop`, `gpu probe` and the Lead-Tutor submission commands (the first upload asks for a user id or creates a random one and remembers it; `dbsloc tutor user-id` shows or changes it).
- Import of NIfTI files and DICOM series (converted with the bundled dcm2niix) into a BIDS dataset, with series suggestions, study grouping and batch import.
- Preprocessing: N4 bias-field correction of the pre-operative anchor image and rigid pre-coregistration to the template.
- Coregistration of post-operative CT or MRI to the pre-operative anchor (multi-start rigid) and registration review slides inside the application.
- Normalization to MNI152NLin2009bAsym with ANTs SyN, multispectral T1w + T2w when a T2w image exists, on a grid that follows the template's voxel size.
- GPU registration engine (Vulkan, DirectX 12 or Metal) for N4, coregistration and normalization, chosen per step, with automatic fallback to the bundled ANTs.
- Electrode reconstruction from post-operative CT (TRAC/CORE on the native CT, PaCER as an alternative) and from post-operative MRI, for 73 electrode models; export of a Lead-DBS compatible `.mat` reconstruction file.
- Orientation detection for directional leads (Boston Scientific and Abbott; experimental).
- Manual correction of a reconstruction in the *Refine localization* view or with `dbsloc reco correct`, with per-lead review.
- Volume of activated tissue and electric field from stimulation settings: stimulation editor, CPU and GPU field solver, Lead-DBS compatible outputs and atlas overlap statistics.
- 3D viewer: bundled DISTAL Minimal and AHEAD atlases, electrode bodies, Lead-DBS default view and mouse bindings, slice backdrops including downloadable 7T ex vivo images, group view.
- Group analyses: group record that reads and writes Lead Group files, batch VTA computation that follows a chosen per-patient stimulation (`dbsloc group calc --sync-settings`), overlap tables, sweet-spot mapping, fibre filtering, network mapping, clinical correlations and non-linear left-right flipping.
- Normative connectomes: catalogue of the Lead-DBS connectomes with licence acceptance, download and import, structural and functional seed maps.
- Import of Medtronic Percept PC / RC session reports (PDF) into stimulation settings, with anonymisation by a patient-constant date shift.
- Bundled ANTs 2.6.5, dcm2niix and the MNI152NLin2009bAsym 1.0 mm T1w / T2w template; further templates downloadable from a checksum-verified catalogue.
- Windows installer `dbs-localizer-setup.exe`: per-user install without administrator rights, Start menu and desktop shortcuts, `dbsloc` on the user's `PATH`.
- Linux: Debian package `dbs-localizer_<version>_amd64.deb`, and a self-contained archive `dbsloc-linux.tar.gz` whose `install.sh` installs under `~/.local` with a menu entry and removes it again with `-u`.
- SHA-256 checksum beside every release download.
- 3D viewer *Fibres* and *Maps* sections: fibre layers drawn as lines of constant pixel width (*Width*, *Maximum shown*, *n of N fibres shown*, colouring by strength), *Add fibre file…* for `.mat` and `.trk` fibre files, and map layers as isosurfaces or slices with a *Threshold*; layers stay when the scene is reloaded.
- Stimulation editor *Connectivity* section: choose a connectome, *Compute connectivity* and *Show fibres* for the open stimulation.
- Group analysis *Analyses* section: sweet-spot mapping, fibre filtering and network mapping from the application (per-patient connectivity maps are computed automatically and recomputed when out of date), with a results list offering *Show in 3D*, *Open folder* and *Delete*.
- Optional ready-to-use copy of a structural connectome (*Keep a copy* in a dialog the first time, *Manage cache…* and the setting *Connectome cache* in *Connectomes…*, `dbsloc connectome cache list|build|clear`): a region of the connectome loads in about 0.1 s.
- User guide (`docs/USER_GUIDE.md`), contributing guide (`CONTRIBUTING.md`) and code of conduct (`CODE_OF_CONDUCT.md`).

### Changed

- Connectomes load faster: Gibbs 19 Full from about 12 s to about 3 s without a copy, and functional connectome runs are read ahead in parallel.
- NIfTI files read and write faster (parallel decoding and compression): a CT loads in about 1.5 s instead of 3.5 s, and importing a `.nii.gz` scan copies it unchanged in about 1 s instead of 13 s.
- Downloaded templates and the 0.2 mm backdrop are stored uncompressed (`t1.nii`, `t2.nii`, `backdrop.nii`), which loads faster; existing `.nii.gz` installs still work.
- *Connectomes…* window shows a list of connectomes grouped by installed / available with filters, and the details of the selected one beside it; licence text and acknowledgement fold away.
- Windows opened with *Open … in separate window* keep the separate-window buttons (*Full screen*, *Dock to main window*) in the same row as the tab's own buttons; the 3D Viewer has one toolbar row.
- *Open folder* buttons on the step cards, after *Export tables*, beside connectivity results, session reports and saved scene images.
- *Connectomes…* comes before *Group analysis* in the top bar; *Run*, *Stop* and *Show results* are narrower.

### Fixed

- Separate windows (group analysis, 3D Viewer, stimulation editor, review, refine) had a black background.
- The *Connectomes…* window could grow wider than the screen and hide its close button.

[unreleased]: https://github.com/WyssCenter/dbs-localizer/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/WyssCenter/dbs-localizer/releases/tag/v0.1.0
