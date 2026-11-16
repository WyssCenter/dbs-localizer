# Connectomes

Fibre filtering and network mapping need a normative connectome: a model of the brain's fibre
tracts (structural) or of correlated activity (functional), built from many healthy people or
patients. Wyss DBS Localizer can download the connectomes that Lead-DBS offers.

## The Connectomes window

Click *Connectomes…* in the header. The window has the list of datasets on the left, grouped as
*Installed*, *Available* and *Other folders* (a coloured dot shows the state), with the filters
*All*, *Installed*, *Structural* and *Functional* above it. Click a dataset to see its details on
the right: where it comes from, its facts and, in folding sections, *Licence and terms*,
*Required acknowledgement* and *How to get this dataset*. Tick *I have read and accept these
terms*, then *Download*. Downloads are checked and can be resumed (*Resume download*). A
connectome that is already installed (for example one imported from Lead-DBS) but has no licence
record, or whose terms changed, shows *Licence and terms* open with the same tick boxes; tick them
and click *Record acceptance* to save your acceptance (the button stays disabled until the
required boxes are ticked). Nothing is downloaded or sent.

- Some datasets are freely available (for example *Global Tracking 19*, 3 GB).
- Some need you to accept the terms of the original study (the HCP-based *dTOR-985* sets, *GSP
  1000*).
- Some can only be imported (*MGH-USC HCP 32*, the *PPMI* sets): Wyss DBS Localizer never
  downloads them, because each user needs their own approved data use agreement with the data
  provider. These datasets show *Import only: needs your own data access* and a numbered list
  under *How to get this dataset* with the application and download pages as links. After you
  have downloaded the archive in your web browser, tick both boxes and choose *Import archive…*
  (or *Choose folder…* for a folder that Lead-DBS already installed). If the archive keeps its
  published file name, its size and checksums are checked before it is extracted; an archive with
  another name is imported without that check, and the result says so.

## Where connectomes are stored

Connectomes are large (from under 1 GB to about 220 GB). They are stored in `connectomes/` in your
user data folder (`%APPDATA%\dbsloc`, `~/.local/share/dbsloc` or
`~/Library/Application Support/dbsloc`); *Change…* at the top of the window picks another place,
for example a large data drive. If you already have Lead-DBS connectomes, see
[Data from a Lead-DBS installation](atlases-templates.md#data-from-a-lead-dbs-installation), or
set *Lead-DBS connectomes folder* (second row at the top of the window, with *Change…* and
*Clear*) to the folder that holds them; the folder you choose there wins over the environment
variable `DBSLOC_LEADDBS_CONNECTOMES_DIR`, and without a choice the window shows the variable's
value marked as coming from the environment variable. The *Import* menu (*Folder…*, *Archive…*)
and the buttons *Choose folder…* and *Import archive…* of a dataset register a copy you already
have.

## Faster loading with a ready-to-use copy

Reading a large structural connectome takes several seconds each time (about 3 s for Gibbs 19
Full). The first time a job uses a structural connectome, a window *Speed up &lt;name&gt;?* offers to
keep a ready-to-use copy on this computer: it needs about 14 bytes of disk space per fibre point
(about 1 GB for Gibbs 19 Full) and later jobs start in a fraction of a second. Choose *Keep a
copy* or *Not now*; the tick box *Don't ask again for this connectome* is ticked by default.
*Keep a copy* is disabled when less than 5 GB would remain free. The results do not depend on the
copy. In the *Connectomes…* window, the *Cache* menu has *Manage cache…*, which lists every copy
with its size and last use, with *Select all*, *Select none* and *Delete selected…*, and lists the
connectomes you declined with *Ask again*. The same *Cache* menu chooses *Ask* (default), *Always*
or *Never*. The copies are stored in the local cache folder of your computer
(`%LOCALAPPDATA%\dbsloc\cache`, `~/.cache/dbsloc` or `~/Library/Caches/dbsloc`), not in the
roaming profile, and can be deleted at any time.

## On the command line

`dbsloc connectome cache list`, `build <id>` and `clear <id>` (or `clear --all`),
`dbsloc connectome list`, `dbsloc connectome licence <id>` and
`dbsloc connectome fetch <id> --accept-terms` for the datasets that can be downloaded. For an
import-only dataset, `fetch` prints the numbered steps and exits with code 2; after you have the
archive, run `dbsloc connectome import <archive> --accept-terms --attest-data-use-agreement`.
`dbsloc connectome map` and `fibervals` compute the seed maps and per-fibre values of a
stimulation ([command line reference](../cli.md#dbsloc-connectome)).

The catalogue, the licence records, the file formats and the seed maps are specified in
[`specs/15_connectomes.md`](../../specs/15_connectomes.md).
