# Importing Medtronic Percept session reports

The session report PDF that a Medtronic Percept PC or RC programmer saves contains the stimulation
settings, impedances and events. Wyss DBS Localizer reads it and turns its stimulation groups into
[stimulation settings](stimulation.md), so you do not have to type them in.

## In the application

Choose *Import… → Session report (PDF)…* in the header (a subject must be open), or *Import
session report…* on the *Volume of activated tissue* card. You can select several PDFs. The
*Import session report* window shows, for each file, the session date, the device, the leads and
the groups.

- *Anonymise* removes the name, patient id, notes and device serial number, and moves every date
  by the same random number of days (between 1 and 30, forwards or backwards) for this patient.
  The time of day, all stimulation values, impedances and events are kept. The time of the import
  is not stored in the record, but the modification times of the files on disk still show when a
  report was imported.
- *Create stimulation settings from* chooses the settings at the start (*Initial*) or end
  (*Final*) of the session, *Both*, or *None*.
- *Groups* chooses which stimulation groups become stimulation labels; *Label prefix* is put in
  front of their names, for example the visit.
- *Keep a copy of the PDF in sourcedata* stores the original (not possible when anonymising).

The PDFs are read on your computer; nothing is sent anywhere. Imported reports are listed under
*Reports* on the *Volume of activated tissue* card.

## The anonymisation key

The date shift comes from a secret key that is created on first use and kept in your user folder
(`%APPDATA%\dbsloc\anonymization.key` on Windows, `~/.local/share/dbsloc/anonymization.key` on
Linux, `~/Library/Application Support/dbsloc/anonymization.key` on macOS). With the same key, all
reports of one patient get the same shift, so intervals between visits stay correct. Back up this
file; with a new key, later imports of the same patient get a different shift. The key and the
shift are never written into the dataset.

## Reports anonymised with version 0.1.0 or an earlier build

For about one patient in 61 the shift was 0 days, so the dates stayed unchanged, and the import
time was stored in every anonymised record. First check that the key file is the one you used
before (`anonymization.key` in your user folder, see above, or the file you chose with
`--key-file`); if the import has to create a new key it warns `W_REPORT_key_created`. Then import
every anonymised report again from its original PDF (an anonymised import keeps no copy of the
PDF) with the same key. For unaffected subjects the record is replaced in place; where you edited
a derived stimulation setting, the import adds a new setting with the suffix `_2` instead of
changing yours. The write time is also removed from the unedited derived settings and from their
`stimparameters.mat` files; the VTA volumes in those files stay as they are (if a file cannot be
cleaned safely, the import warns `W_REPORT_stamp_kept`; run the vat step again with
`--overwrite-approved` to rewrite it, or, for a subject without VTA results (the command writes
the native `.mat` with zero volumes, so it is only for that case), save the setting again with
`dbsloc stim show <dataset_root> <subject> <label> --json > label.json` and
`dbsloc stim set <dataset_root> <subject> <label> --from label.json`; the warning repeats on every
anonymised import while a file stays uncleaned). Because the settings files change, an approved
*Volume of activated tissue* step becomes unapproved when you use fingerprinted locks: approve it
again or run it again. For an affected subject the record is written again with shifted dates,
the earlier record that had kept the true dates is removed, and the import warns
`W_REPORT_superseded` with the number of stimulation settings created from it that remain. Their
names carry the earlier date, and so do their files, so renaming does not help: list them with
`dbsloc stim list <dataset_root> <subject>`, delete them, and create again under a new name the
settings you still need (`dbsloc stim set`, or the stimulation panel). Make files produced
earlier with `dbsloc report anonymize` again, and replace copies you already shared.

## Supported reports

So far only Medtronic Percept PC and RC reports are supported, and they have been tested with
English reports only.

## On the command line

```bash
dbsloc report inspect report.pdf                                    # show what would be read, write nothing
dbsloc report import my-study 01 report.pdf --anonymize --snapshot final
dbsloc report list my-study 01
```

The record format, the anonymisation and the Medtronic layout are specified in
[`specs/16_session_reports.md`](../../specs/16_session_reports.md) and
[`specs/16a_medtronic_percept_layout.md`](../../specs/16a_medtronic_percept_layout.md).
