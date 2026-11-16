# Accuracy and Lead-Tutor

## Accuracy

```{include} ../../README.md
:start-after: <!-- docs:accuracy -->
:end-before: <!-- /docs:accuracy -->
```

## Checking the accuracy yourself

[Lead-Tutor](http://tutor.lead-dbs.org) is a service of the Lead-DBS team for learning and
checking electrode reconstructions. It holds expert reconstructions of 10 patients whose scans are
public. You upload your reconstructions, and its feedback page shows how far each lead is from the
expert's; the expert positions themselves stay hidden. Anyone can use it, so you can check the
accuracy reported for Wyss DBS Localizer yourself:

1. **Download the data.** Get `LeadTutor.zip` (about 900 MB) from the Open Science Framework (OSF)
   project [jxecg](https://osf.io/jxecg/) and extract it. The folder that contains
   `dataset_description.json` is the dataset; it already records the electrode model of each
   patient.
2. **Run the pipeline** for all 10 patients, without stopping for reviews:

   ```bash
   dbsloc run LeadTutor --all-subjects --headless --no-checkreg
   ```

   With a graphics card this takes roughly 10 to 15 minutes in total.
3. **Your Lead-Tutor user id.** Lead-Tutor files uploads under a user id, so you can compare your
   runs. The first upload asks you for one (letters, digits, `.`, `_` and `-`, for example
   `jdoe-dbsloc-2026`); press Enter to have a random id created instead. The id is saved and used
   for every later upload. `dbsloc tutor user-id` shows it, and `dbsloc tutor user-id --set <id>`
   changes it. Pick an id nobody else uses.
4. **Check, then upload:**

   ```bash
   dbsloc tutor submit-all-recon LeadTutor --require-all --dry-run   # show what would be sent
   dbsloc tutor submit-all-recon LeadTutor --require-all
   ```

   Only the template-space (MNI) coordinates of the two lead ends and of a point marking the
   orientation, for the 10 tutorial patients, are sent; no images, names or other data. The
   upload goes to port 8000 of `tutor.lead-dbs.org`; some institutional networks block that port.
5. **Read the result.** The feedback page (`http://tutor.lead-dbs.org/?userid=<your-id>`) opens in
   the browser. It shows, for each patient and lead, how far your lead is from the expert's. The
   service measures two distances per lead: at the deepest contact level (head) and at the 4th
   contact level (tail; the top level of the 4-level leads in the Lead-Tutor cohort). The accuracy
   above is, for each lead, the average of these two distances, averaged over all 20 leads.

The team's own upload of the version described above scored 1.15 mm on this measure. Small
differences between computers and graphics cards are expected.

`dbsloc tutor --help` lists the other upload commands (`submit-recon` for one subject,
`submit-file` for a prepared file).
