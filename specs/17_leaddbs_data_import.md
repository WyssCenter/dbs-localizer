# 17 — Import of data from an existing Lead-DBS installation

**Status:** normative draft (2026-11-10, owner request of the same date). Sources: this module reads **data folders** that a Lead-DBS user already has on disk (atlas folders, connectome folders); it reads no Lead-DBS code. Folder layouts are those of `specs/11` §1–§2 and `specs/15` §3.

## 1. Purpose

A user who already works with Lead-DBS has atlases and normative connectomes on disk, some of them tens of gigabytes and some obtained under their own data use agreement. Wyss DBS Localizer must be able to use them without a second download. On the first start the application asks whether to import data from a Lead-DBS installation; the user picks the folder, sees what was found and how large it is, and chooses between using the data where they are and copying them.

Nothing is ever written, moved or deleted inside the Lead-DBS folder.

Out of scope for this version: templates (the bundled template and the catalogue of `specs/04` §3.1 cover them), patient data, Lead-DBS preferences, tract atlases other than atlas folders.

## 2. Scan

`scan(dir)` takes the folder the user chose and returns an inventory. It accepts any of:

- a Lead-DBS installation folder (it contains `templates/space/`);
- a Lead-DBS data folder that contains `connectomes/`;
- a connectome root itself (it contains at least one of `dMRI/`, `fMRI/`, `dMRI_MultiTract/`).

Resolution:

- **Atlases root** = `<dir>/templates/space/MNI152NLin2009bAsym/atlases` when that is a directory, else none.
- **Connectome root** = `<dir>/connectomes` when it contains at least one class folder, else `<dir>` when it contains at least one class folder, else none.
- The path is cleaned as in `atlas_catalog` (`~` expansion, `..` components refused, canonicalised). A missing or unreadable folder is `E_LDBS_MISSING` "<path> is not a folder that can be read".
- Neither root found: not an error; the inventory is empty and carries the note "No Lead-DBS atlases or connectomes were found in <path>. Choose the Lead-DBS installation folder (it contains `templates`) or the folder that contains `connectomes`."

Items:

- **Atlas item**: every immediate subfolder of the atlases root whose name does not start with `.` (`specs/11` §1.1). Fields: `name` (folder name), `path`, `bytes`, `files`, `has_index` (`atlas_index.mat` present), `bundled` (an atlas set of the same name ships with the application, `resources/atlases/leaddbs/<name>`).
- **Connectome item**: every folder the discovery rule of `specs/15` §3.1 recognises under the connectome root (class folder + marker file). Fields: `class`, `name`, `path`, `bytes`, `files`, `catalogue_id` and `access` when the folder matches a catalogue entry (`specs/15` §3.1 *Catalogue match*), `present` (a connectome of the same class and name is already in the primary connectome root or registered).
- `bytes` and `files` are the recursive sum over regular files; symbolic links and reparse points are not followed and count as 0. An unreadable entry is skipped and counted in `unreadable` of its item.
- Items are sorted by name (bytewise) within atlases and within each connectome class.
- The scan is cancellable (an atomic flag checked per directory) and reports progress as the number of items sized so far; it performs no network access and writes nothing.

Inventory: `{ source, atlases_root, connectome_root, atlases, connectomes, notes }` with totals derivable from the items.

## 3. Using the data

Two modes. The choice applies to the whole import; the selection of items applies to the copy mode only.

### 3.1 Use in place (*link*)

- The application stores two settings: *Lead-DBS atlases directory* = the atlases root (the existing setting `leaddbs_atlases_dir`, `specs/11` §1) and *Lead-DBS connectomes folder* = the connectome root (the GUI setting named in `specs/15` §3.1, `leaddbs_connectomes_dir`; it takes precedence over `DBSLOC_LEADDBS_CONNECTOMES_DIR`).
- Nothing is copied; no disk space is used. Every atlas and connectome folder of the two roots is listed by the existing discovery, read in place.
- A root that is none leaves its setting unchanged.
- If the Lead-DBS folder is later moved or removed, the atlases and connectomes disappear from the lists; the existing rules for a missing directory apply.

### 3.2 Copy

- Destinations: an atlas goes to `<user data dir>/atlases/leaddbs/<name>/`; a connectome to `<primary connectome root>/<class>/<name>/` (`specs/15` §3.1).
- Default selection: every atlas with `bundled = false` and every connectome with `present = false`. The user can change it per item.
- Before the first byte: the free space of each destination volume must be at least the selected bytes for that volume plus 1 GB, else `E_LDBS_DISK` "not enough free disk space: need <N> GB, <M> GB free on <volume>" and nothing is copied.
- Each item is copied into a staging folder beside its destination (`.partial-copy-<name>`), file by file, without following links, then renamed into place. Only regular files and directories are copied. A destination folder that already exists is not touched: the item is reported as *already there* and skipped.
- Progress events: item started (name, index, count), bytes copied of total, item done, all done. Cancelling removes the staging folder of the current item; items already renamed into place stay.
- A read or write error stops the import with `E_LDBS_IO` (path and reason), removes the current staging folder and keeps completed items.
- After at least one atlas was copied, the setting *Lead-DBS atlases directory* is set to `<user data dir>/atlases/leaddbs`. Copied connectomes are in the primary root and need no setting.
- Staging folders older than 7 days are removed at the next copy.
- Nothing is ever written inside the Lead-DBS folder. Before anything is created, each destination directory is resolved (through its existing ancestors); a destination inside the scanned folder, inside the atlases root or the connectome root, or a selected item whose source lies inside a destination, is refused with `E_LDBS_IO` "the destination <path> is inside the Lead-DBS folder <path>; choose a data directory outside it".
- Links are never followed: a symbolic link or a directory junction or mount point is skipped and counted, and the outcome reports the count per item (`skipped_links`; CLI and GUI print "N linked entries were not copied"). A regular file that carries the Windows reparse-point attribute (cloud-storage placeholder, deduplicated file) is a regular file: it is counted and copied, and a read failure on it is `E_LDBS_IO` for a copy and an unreadable count for a scan.
- When a copy stops early (cancel or error), the error carries a partial outcome with the items completed and skipped so far and the atlases directory; the callers store the setting from it when at least one atlas was copied and list what was copied before the stop.
- Every staging folder holds a marker file `.dbsloc-copy.json` (process id and a heartbeat time). The copy refreshes the heartbeat at most every 30 seconds while it copies, so a live copy never looks stale. The sweep and the removal of a pre-existing staging folder of the same name touch only folders with a marker; an unmarked folder is left alone and fails that item with `E_LDBS_IO`. A marked folder whose heartbeat is older than 10 minutes is stale (left by a crash, a forced quit or a kill): it is removed and created again without an error. A fresher one fails the item with `E_LDBS_IO` "another copy of <name> seems to be running (folder <path>, last active <N> minutes ago). If no other copy is running, delete that folder and try again." There is no special case for the same process: jobs are de-duplicated, so a process never copies one item twice at once. If the marker cannot be written after the folder was created, the folder is removed again before the error is returned. After a cancel or an error the staging folder is removed with a few retries and a short pause (virus scanners on Windows hold files briefly); if it still cannot be removed, the partial outcome carries the note "the temporary folder <path> could not be removed; delete it before copying <name> again".
- Only the destinations that a selected item uses are checked against the Lead-DBS folder: copying atlases alone is not refused because the connectome root lies inside the Lead-DBS folder, and the reverse.

### 3.3 Licence records

The import writes no licence record. A connectome that matches a catalogue entry of class `dua` or `cohort_terms` is marked in the inventory (*Needs your data use agreement* / *Cohort data terms*); it is usable at once, and the *Connectomes* window records the acceptance (`specs/15` §4.3). The result summary names these connectomes and says where to record it: "Open Connectomes…, select the connectome and tick the boxes under Licence and terms to record your acceptance."

The Lead-DBS atlases come with the request of their owners not to use them commercially (`docs/ATLAS_DISTAL.md`); the dialog shows one line saying so above the atlas list.

## 4. Interfaces

### 4.1 Pipeline (`crates/dbsloc-pipeline/src/leaddbs_data.rs`)

- `scan(dir, context, cancel, on_progress) -> Result<Inventory, LeadDbsDataError>`; `context` gives the bundled atlas names and the connectome roots used for `bundled` / `present`.
- `link_plan(&Inventory) -> LinkPlan { atlases_dir: Option<PathBuf>, connectomes_dir: Option<PathBuf> }`.
- `copy(&Inventory, &Selection, &Destinations, cancel, on_event) -> Result<CopyOutcome, LeadDbsDataError>`; `CopyOutcome` lists copied, skipped (*already there*) and the atlases directory to store.
- Error codes: `E_LDBS_MISSING`, `E_LDBS_DISK`, `E_LDBS_IO`, and "cancelled".
- A selection name that is not in the inventory is `E_LDBS_MISSING`. An atlas or connectome folder that is itself a symbolic link or junction is not listed; the inventory carries a note naming it.

### 4.2 CLI

```
dbsloc leaddbs scan <dir> [--json]       what was found: kind, name, size, files, notes
dbsloc leaddbs copy <dir> [--all | --atlas NAME ... --connectome NAME ...] [--root DIR] [--json]
```

- `scan` prints one table for atlases and one for connectomes with sizes in MB / GB and the totals; exit 0 also when nothing was found (the note is printed), 1 on `E_LDBS_MISSING`.
- `copy` without a selection uses the default selection of §3.2; `--root` is the primary connectome root (`specs/15` §3.1). A name that is not in the inventory is a usage error (exit 2) that lists the names.
- Without a per-user data folder `scan` still works (every `present` is false). With `--json`, every failure that has no partial outcome prints `{"error": "<code>: <message>"}` on standard output; the exit code is unchanged. A scan and the copy after it share one Ctrl-C flag.
- For the link mode the CLI needs no command: `scan` ends with the two lines to set (`DBSLOC_LEADDBS_ATLASES_DIR=<path>`, `DBSLOC_LEADDBS_CONNECTOMES_DIR=<path>`), each only when its root was found.

### 4.3 GUI

**First start.** The saved application state has a flag `leaddbs_import_offered` (default false). When it is false at start, a window *Import data from Lead-DBS* opens over the start screen:

> Do you already use Lead-DBS on this computer? Wyss DBS Localizer can use its atlases and connectomes, so you do not have to download them again.

Buttons: *Choose Lead-DBS folder…* and *Skip*. Below: "You can do this later from *Connectomes…* → *Import* → *From a Lead-DBS installation…*". The flag is set at the moment the window opens, so the question is asked once even when the application is quit with the window open. It is not shown in screenshot or headless modes that open another screen directly.

**Steps of the window** (one window, no nested dialogs):

1. *Choose folder*: the text above; a folder dialog (the existing `FolderPick`).
2. *Scanning*: a spinner with the count of items sized and *Cancel*.
3. *What was found*: the path of the inventory that is shown (a new folder replaces it only when its scan succeeds; a cancelled, failed or vanished scan returns here with the old path and inventory, and a vanished scan worker shows "The scan stopped unexpectedly.") with *Choose another folder…*; the notes of the scan are always listed under it, in muted text, so a linked folder that was left out is visible; two groups, *Atlases (N, total size)* and *Connectomes (N, total size)*, each a scrollable list of rows: name, kind (for connectomes *Structural* / *Functional*), size, and a note (*Already included*, *Already there*, *Needs your data use agreement*, *Cohort data terms*). An empty inventory shows the note of §2 instead.
4. The choice, as two radio buttons with one explanatory line each:
   - *Use them where they are*: "No extra disk space. They stay in the Lead-DBS folder; if you move or remove Lead-DBS they are no longer available here."
   - *Copy them into Wyss DBS Localizer*: "Needs <N> GB (<M> GB free). They stay available when Lead-DBS is removed."
   With *Copy* chosen, each row gets a tick box (default selection of §3.2) and the needed size follows the selection; when the free space is too small the *Import* button is disabled with the reason. The needs of the two destinations are summed when they are on one disk volume (decided once, in the scan worker) and checked separately otherwise. Sizes below 1 MB are shown in kB. *Esc* closes the window only when the window has keyboard focus or the pointer is over it, no other popup is open and no job runs.
5. *Import* runs the mode: link is immediate; copy shows a progress bar (item n of N, bytes, *Cancel*); a copy worker that disappears is a failure whose partial outcome is the list of items the window already saw complete.
6. *Done*: what is now available (counts, sizes, where), skipped items, the connectomes that need a licence record with the button *Open Connectomes…*, and *Close*.

Other entry points: *Connectomes…* window → *Import* menu → *From a Lead-DBS installation…*; the *Lead-DBS atlases directory* row of the pipeline panel gets the same button. The *Connectomes* window shows the setting *Lead-DBS connectomes folder* with *Change…* and *Clear* (it replaces the environment-variable-only note).

Rules of `docs/agent-rules/gui-ux.md` apply: every control has a tooltip, long work runs off the UI thread, no unexplained abbreviations, the flag and the two settings are persisted with the application state.

## 5. Acceptance criteria and tests

Fixtures are built in temporary folders by the tests (small files); no real Lead-DBS data.

1. A folder with `templates/space/MNI152NLin2009bAsym/atlases/{A,B,.hidden}` and `connectomes/dMRI/X/data.mat`, `connectomes/fMRI/Y/dataset_info.json` scans to atlases `A`, `B` and connectomes `X` (dMRI), `Y` (fMRI) with exact byte and file counts; `.hidden` is absent.
2. The data folder that contains `connectomes/` and the connectome root itself both resolve to the same connectome root; a folder with neither returns the empty inventory with the note; a missing folder is `E_LDBS_MISSING`; a path with a `..` component is refused.
3. `bundled` is true for an atlas whose name is in the bundled names; `present` is true for a connectome already in the primary root; a folder named like a catalogue entry carries its `catalogue_id` and `access`.
4. `link_plan` returns the two roots and nothing is written anywhere (the source tree's file list and modification times are unchanged after scan and link).
5. Copy with the default selection copies exactly the non-bundled atlases and non-present connectomes, byte-identical, into the destinations of §3.2; an existing destination folder is skipped and reported; the source tree is unchanged.
6. Copy with too little free space (injected free-space value) fails with `E_LDBS_DISK` before anything is created.
7. Cancel during a copy leaves no staging folder and keeps completed items; a symbolic link in the source is not followed and not copied.
8. CLI: `scan` table and `--json`, `copy --all`, an unknown `--atlas` name exits 2 and lists the names.
9. GUI: the state flag round-trips and defaults to false; an old state file without the flag loads; the first-start window is requested exactly once; the window draws without panicking in each step at wide and narrow sizes; the copy button is disabled with a reason when space is short; the two settings are stored by the link mode and the atlases directory by the copy mode.

## 6. Decisions (2026-11-10)

| # | Decision |
|---|----------|
| D1 | Asked once at first start, and available later from the *Connectomes* window and the atlases row. |
| D2 | Two modes for the whole import: use in place (settings only) or copy (per-item selection). No moving, no deleting of the source. |
| D3 | Copied atlases live in `<user data dir>/atlases/leaddbs` and are reached through the existing *Lead-DBS atlases directory* setting; there is one such directory at a time. |
| D4 | The import records no licence acceptance; restricted connectomes are flagged and the acceptance is recorded in the *Connectomes* window. |
| D5 | Templates are not imported in this version. |
