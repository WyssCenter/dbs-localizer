# Spec 15 — Normative connectomes: data, download, formats and per-subject maps (Wyss DBS Localizer)

**Status:** Phase B, 2026-10-23. **Scope owner:** the connectome side of Lead Group parity: which connectomes exist, how they are obtained (download, licence acceptance, import), how they are read, and the per-subject computations (structural seed maps, functional seed maps, per-fibre values). Group statistics (fibre filtering t-tests / correlations, network-mapping R-maps, sweet spots, cross-validation) are in `specs/14_lead_group.md`; stimulation volumes and E-fields (the seeds used here) are in `specs/07_vat.md`. This spec refers to both by intent and does not repeat them.

**Sources.** Phase-B reading of the reference implementation (Lead-DBS, commit `5b1008d7`, 2026-03-02): `ea_checkinstall.m`, `helpers/ea_getassetfilesize.m`, `helpers/ea_downloadasset_parallel.m`, `helpers/ea_getconnectomebase.m`, `helpers/ea_getConnLabel.m`, `connectomics/ea_loadfibertracts.m`, `ea_savefibertracts.m`, `ea_trk2ftr.m`, `ea_genmodlist.m`, `connectomics/mapper/{ea_lcm,ea_lcm_struc,cs_dmri_conseed,cs_dmri_conseed_map,cs_fmri_conseed_seed_tc,ea_conformseedtofmri}.m`, `explorers/fiberfiltering_explorer/{ea_disctract,ea_discfibers_calcvals,ea_discfibers_getvats,ea_discfibers_merge_pathways}.m`, `explorers/networkmapping_explorer/{ea_networkmapping_calcvals,ea_networkmapping_getvats}.m`, `common/ea_prefs_default.m`. No code is reproduced; behaviour is described as prose, tables and mathematics. Network facts were checked on 2026-10-23 with a handful of HEAD / PROPFIND / small `Range` requests (no full download); owner decisions: `specs/14_lead_group.md` §15.

**Unverified** marks every statement that could not be checked against a file, a server response or a published licence.

---

## 1. Purpose, scope, terminology

### 1.1 Purpose

Give an implementer who works from this specification and the cited sources everything needed to (a) list, download or import the normative connectomes that Lead-DBS users work with, honouring each dataset's terms; (b) read the Lead-DBS on-disk formats natively; (c) compute, per subject and per stimulation, the structural seed map, the functional seed map and the per-fibre values that the group analyses of `specs/14` consume.

### 1.2 In scope / out of scope

| In scope | Out of scope (owner) |
|----------|----------------------|
| Catalogue of downloadable connectomes, licence display and acceptance records, polite downloader, import of user-obtained folders (§2, §4) | Group statistics, R-maps, fibre t-tests, cross-validation (`14`) |
| Readers: structural `data.mat` (MAT v5 / v7, MAT v7.3), `.trk` (TrackVis), functional `dataset_info.json` + `dataset_volsurf.mat` + per-run time-course files; multi-tract folders (§3) | VTA / E-field computation (`07`) |
| Structural seed map from a VTA / E-field seed (§5) | Patient-specific tractography and patient rs-fMRI preprocessing |
| Functional seed map (Pearson r, Fisher z, mean, variance, t) (§6) | `fMRI_matrix` connectomes (precomputed voxel × voxel `AllX.mat`): listed, refused for maps in v1 (**deferred**) |
| Per-fibre values for fibre filtering, fibre subset files for the viewer (§7) | Surface (`surf/`) time courses of functional connectomes (`prefs.lcm.includesurf` defaults off in the reference): ignored in v1 |
| Per-subject inputs of network mapping (§8), job contract and output names (§9) | DSI Studio `.fib.gz` on-demand tracking (the reference calls DSI Studio): not supported |
| | PAM / OSS-DBS fibre activation (`07`, `14`) |

### 1.3 Terminology

| Term | Meaning |
|------|---------|
| **Structural connectome** | A set of K streamlines (fibres), each a polyline of points in template-space millimetres, typically from group tractography of diffusion MRI. Lead-DBS folder class `dMRI`. |
| **Multi-tract connectome** | A folder of several structural fibre files, one per named pathway (Lead-DBS folder class `dMRI_MultiTract`), merged with global fibre numbering for analysis. |
| **Tract atlas** | Curated pathways shown as atlas structures (e.g. Macroscale Human Connectome Atlas, Yeh 2018). Handled by `specs/11` (atlas folders), not by this spec, except that a multi-tract connectome built from one is read as in §3.4. |
| **Functional connectome** | Resting-state BOLD time courses of N voxels for many subjects and runs in a fixed 2 mm template grid (`fMRI_timecourses`), or a precomputed voxel × voxel mean correlation (`fMRI_matrix`, deferred). Lead-DBS folder class `fMRI`. |
| **Subset** | A named list of subject indices of a functional connectome (`dataset_info.json` `subsets`), e.g. *Full Set*, *Patients*. |
| **Seed** | A NIfTI image in template space whose non-zero voxels (with their values as weights) define the region whose connectivity is mapped; here built from a subject's VTA / E-field (`07`). |
| **Seed map** | The per-subject output image: structural *strucmap* (fibre-density through the seed) or functional *funcmap* (correlation statistics). |
| **Per-fibre value** | For one subject, side and connectome: one number per fibre (binary hit, E-field sum / mean / peak / top-5 % mean) — the input of fibre filtering. |
| **Connectome root** | A folder laid out like a Lead-DBS `connectomes/` folder: `dMRI/`, `fMRI/`, `dMRI_MultiTract/`. |
| **Tag** | The short identifier from `dataset_info.json` `tag` used in output file names (`conn-<tag>`). |

---

## 2. Catalogue of connectomes known to Lead-DBS

### 2.1 How Lead-DBS downloads today (verified 2026-10-23)

- The installer's menu (`ea_checkinstall`) lists, among other assets, six connectomes and one tract atlas; the website's *Normative Connectomes* page (`https://www.lead-dbs.org/?p=7692`) lists three more (dTOR-985, Pediatric rs-fMRI, GSP 1000 v2).
- Download ids are **strings**, not numbers: `https://www.lead-dbs.org/release/download.php?id=<id>`. A numeric id (`id=1`) returns **404**, which explains the earlier failed check. A valid id returns **HTTP 302** with `Location: https://uni-koeln.sciebo.de/public.php/dav/files/8s7cxAWA4PA5GZq/data/<file name>` — a public Nextcloud (sciebo, University of Cologne) share accessed over WebDAV. The 302 response itself carries no length.
- The share answers `HEAD` and `GET` anonymously with `200`, `Content-Length`, `ETag`, `Last-Modified`, `Content-Disposition` and **`OC-Checksum: SHA1:<hex>`**; it honours `Range` (a `bytes=0-15` request returned `206 Partial Content` with `Content-Range: bytes 0-15/4289260330`), although the `HEAD` response does not advertise `Accept-Ranges`. A single `PROPFIND` (Depth 1) on the share's `data/` folder lists every file with size, modification time and SHA-1 (used for the table in §2.2).
- The reference implementation's size probe (`ea_getassetfilesize`) still parses a legacy pCloud listing (`https://filedn.com/lsPIJ4ragTWjjmV6PvlDQLu/data/`); the website still links *older versions* there (`…/data/older_versions/…`). Whether filedn.com still serves files is **unverified**; Wyss DBS Localizer does not use it.
- GSP 1000 v2 is not on lead-dbs.org: it is a Harvard Dataverse dataset (doi:10.7910/DVN/KKTJQC, version 4.0, released 2025-05-19, 38 files). The Dataverse access API `https://dataverse.harvard.edu/api/access/datafile/<file id>` answers anonymously with **303** to a pre-signed Amazon S3 URL (valid 3600 s) that serves `Accept-Ranges: bytes`; per-file MD5 values are published in the dataset metadata (`/api/datasets/:persistentId/?persistentId=doi:10.7910/DVN/KKTJQC`). Files are not restricted; no guestbook was visible in the metadata (**unverified** whether the web UI shows a terms pop-up that the API skips).

### 2.2 Catalogue (normative content of `connectome_catalog.rs`)

Sizes are `Content-Length` / `PROPFIND` values; SHA-1 from the share's `OC-Checksum`; all checked 2026-10-23. "Folder" is the top-level folder inside the archive that the reference implementation expects under `<root>/<class>/`.

| Our id | Lead-DBS name (folder) | Class / type | Space | Cohort | Lead id → file | Bytes | SHA-1 (source) | Version |
|--------|------------------------|--------------|-------|--------|----------------|------:|----------------|---------|
| `horn2013-gibbs19` | `Global Tracking 19 (Horn 2013) Full` (zip inspected 2026-10-24: seven top-level folders, `… Full` and `… Thinned Out 2 / 5 / 10 / 50 / 100 / 500`, each with `data.mat`, `dataset_info.json`, `dataset_info.mat`; §4.6) | dMRI, Gibbs tracking | MNI152NLin2009bAsym (**unverified** for this oldest set) | 19 healthy subjects | `group2013` → `groupconnectome_horn_2013.zip` | 3 015 912 949 | `f738d3e7d02c8e0b9a525bfd4f1ceb9adb7af2a1` | 1.0 |
| `horn2016-nkirs169` | `NKI-RS 169 (Horn 2016)` (installer code) | dMRI, Gibbs tracking | MNI152NLin2009bAsym | NKI Rockland Sample, 169 | `group2016` → `groupconnectome_nki_169_horn_2016.zip` | 7 325 698 796 | `170e793e2f7cf27247bb41e5c1842b8d82b38940` | 1.0 |
| `horn2017-mghusc-hcp32` | `MGH-USC HCP 32 (Horn 2017)` (zip inspected) | dMRI, GQI (DSI Studio) | MNI152NLin2009bAsym | HCP MGH-USC Adult Diffusion, 32 | `group2017` → `groupconnectome_HCP_MGH_32fold_horn_2017.zip` | 4 289 260 330 | `422eb83ccf4e540b27e23873524b6a60d0e41ef7` | 1.2 (01/2020, `version_and_license.txt`) |
| `ewert2017-ppmi85` | `PPMI 85 (Ewert 2017)` (installer code) | dMRI, GQI | MNI152NLin2009bAsym | PPMI Parkinson patients, 85 | `group2017_ppmi` → `groupconnectome_ppmi_85_ewert_2017.zip` | 685 944 962 | `ae94d8f0d8b58c83202abde75de5c4c5e0bc1cd7` | 1.1 |
| `elias2024-dtor985-quarter` | dTOR-985 Quarter (folder **unverified**) | dMRI, GQI, ~12 M streamlines full set | MNI152NLin2009bAsym | HCP WU-Minn young adults, 985 | `dTOR985_Quarter` → `dTOR-985_Quarter.zip` (id → file mapping from the website and listing; not HEAD-checked) | 2 886 958 128 | `f723272beb82f3785bff7c8f65b74ea87d85f4ff` | figshare v2 |
| `elias2024-dtor985-half` | dTOR-985 Half (folder **unverified**) | as above | as above | as above | `dTOR985_Half` → `dTOR-985_Half.zip` (not HEAD-checked) | 5 776 636 849 | `ebbefbe81743c6de909dd814bb91995f3566b256` | figshare v2 |
| `elias2024-dtor985-full` | dTOR-985 Full (folder **unverified**) | as above | as above | as above | `dTOR985_Full` → `dTOR-985_Full.zip` (HEAD-checked) | 11 551 215 059 | `74ab1985ffbec09ce5418f08b8e143c772f312f4` | figshare v2 |
| `horn2017-ppmi74p15c` | `PPMI 74 Patients 15 Controls (Horn 2017)` (zip inspected; tag `PPMI74P15C`) | fMRI_timecourses, 89 subjects, subsets *Full Set*, *Patients*, *Controls*, *Patients, 1st Half*, *Patients, 2nd Half*; has `surf/` | MNI152NLin2009bAsym 2 mm | PPMI | `fgroup2017_ppmi` → `functional_groupconnectome_ppmi_74_15_horn_2017.zip` (273 members) | 8 042 457 187 | `5c7b578ca2bfb693b42b6feb3b218ef7cd1bb43d` | 1.0 |
| `loh2020-torpd75` | `Tor PD (Loh & Boutet 2020)` (installer code) | fMRI_timecourses | MNI152NLin2009bAsym 2 mm | Toronto PD, 75 patients, 77 acquisitions | `TorPD` → `TorPD_Loh_Boutet_2020.zip` (HEAD-checked) | 25 665 221 855 | `13768da336ade8e6d3b92a229bafc289d9d3aa59` | 1.0 |
| `alfatly2023-pediatric100` | Pediatric rs-fMRI (folder **unverified**) | fMRI_timecourses | **MNI pediatric template**, not 2009b (**unverified** which exact space id) | CoRR NYU 2, 100 children 6–18 y | `PedrsfMRI` → `Pediatric_rsfMRI.zip` (HEAD-checked) | 17 008 253 598 | `987c3a92f5a8c46c312c2c07ed467788c3a721b9` | 1.0 |
| `cohen2023-gsp1000v2` | `GSP 1000 v2 (Cohen 2023)` (from `dataset_info.json`; tag `GSP1000v2`) | fMRI_timecourses, 1000 subjects, 1–2 runs each; subsets *Full Set*, *First 100 Subjects*, *First 5 Subjects*, *First Subject*; no `surf` | MNI152NLin2009bAsym 2 mm | Brain Genomics Superstruct Project | Dataverse, 38 files (Appendix A) | 218 932 842 997 total | MD5 per file (Appendix A) | Dataverse 4.0 |

Not offered for download but recognised when present in a connectome root: `GSP 1000 (Yeo 2011)` and its `Groupmatrix` variant (older Lead-DBS releases; the `Groupmatrix` one is `fMRI_matrix`, deferred), any user-made folder in the same layout. The tract atlas `Macroscale Human Connectome Atlas (Yeh 2018)` (`macroscalehc` → `Macroscale_Human_Connectome_Atlas_Yeh_2018.zip`, 1 291 249 577 bytes, SHA-1 `ceff214b06105cc820a38bed16fb38a76c342545`) belongs to `specs/11` (atlas folder), not to this catalogue.

### 2.3 Licences, redistribution and what the user must do

Access classes (catalogue field `access`):

- **`open`** — the derived files carry an open licence or none is stated, and the source cohort imposes no click-through terms on derived data. The downloader may fetch after the user acknowledges the licence / citation text shown.
- **`cohort_terms`** — the source cohort has open-access data use terms that a user accepts by agreeing (no application) and that explicitly allow redistribution of derived data "under these same Data Use Terms". The downloader may fetch after the user accepts those terms, shown verbatim.
- **`dua`** — the source cohort requires each user to hold their own approved data use agreement (an application through a portal). **Import-only since 2026-11-10 (§2.4, §11.1 D10):** the entry stays in the catalogue with its citation, terms and numbered step-by-step instructions (`steps`, §4.1), but Wyss DBS Localizer never downloads it. The user obtains the archive themselves after the cohort owner approved them, then imports it (*Import archive…* / *Choose folder…*, §4.3). The attestation tick remains: it is recorded with the licence record at import.

| Id | Licence of the Lead-DBS file | Source-cohort terms | Redistribution of derived data | Access class / user action | Confidence |
|----|------------------------------|---------------------|-------------------------------|----------------------------|------------|
| `horn2013-gibbs19` | None stated | Local research cohort (Horn 2013); no public terms found | Not stated (**unverified**) | `open`: acknowledge citation; download | Low |
| `horn2016-nkirs169` | None stated | NKI-RS / INDI: Creative Commons Attribution-NonCommercial (per INDI usage page; full text on rocklandsample.org not read) | Allowed with attribution, non-commercial | `open` + notice *non-commercial use only*; download | Medium |
| `horn2017-mghusc-hcp32` | `version_and_license.txt` points to the MGH HCP data agreement | *HCP Data Use Agreement* (MGH-USC, LONI IDA, 2014): "will not further disclose these data beyond the uses outlined in this agreement and my data use application"; sharing only with people who comply; acknowledgement wording mandatory | Restricted to holders of the agreement | `dua`: link `https://ida.loni.usc.edu/` (HCP MGH-USC Adult Diffusion collection) + `https://www.humanconnectome.org/storage/app/media/documentation/mgh_diffusion/HCP_Data_Agreement.pdf`; instructions: apply for access, then either attest and download, or download from lead-dbs.org yourself and *Choose folder…* | Medium-high (agreement text read) |
| `ewert2017-ppmi85`, `horn2017-ppmi74p15c` | None stated | PPMI Data Use Agreement (registration and application through `https://www.ppmi-info.org/access-data-specimens/download-data`, LONI IDA); individual-level data may not be redistributed (PPMI staff statement; DUA v3.0 text read only in a 2018 copy) | Not allowed outside the agreement | `dua`: link + instructions as above | Medium |
| `elias2024-dtor985-*` | **CC0 1.0** (figshare collection doi:10.6084/m9.figshare.c.6844890, article *dTOR-985 full connectome (lead-dbs version)*) | WU-Minn HCP Open Access Data Use Terms: "I may redistribute original WU-Minn HCP Open Access data and any derived data as long as the data are redistributed under these same Data Use Terms"; acknowledgement wording | Allowed under the same terms | `cohort_terms`: show the WU-Minn terms verbatim (`https://www.humanconnectome.org/study/hcp-young-adult/document/wu-minn-hcp-consortium-open-access-data-use-terms`); accept; download | High |
| `loh2020-torpd75` | **CC BY 4.0** (Zenodo doi:10.5281/zenodo.4310183; the Zenodo zip is a different package of 12 831 318 183 bytes) | Toronto cohort, released by the authors | Allowed with attribution | `open`; download | High |
| `alfatly2023-pediatric100` | None stated | CoRR / INDI NYU 2 — licence **unverified** (INDI releases are commonly CC BY-NC) | **Unverified** | `open` + notice *check CoRR terms*; download | Low |
| `cohen2023-gsp1000v2` | Dataverse *Terms of Use*: "Access and use of this derivative dataset is permitted solely under the terms of the original GSP data release" | GSP Open Access Data Use Terms (version 2014-Apr-22), item 5: "I may redistribute original GSP Open Access data and any derived data as long as the data are redistributed under these same Data Use Terms"; item 6 acknowledgement wording | Allowed under the same terms | `cohort_terms`: show the terms verbatim; accept; download from Dataverse (Appendix A) | High |

The *Access class / user action* column above is superseded for the `dua` rows, the dTOR-985 source and the two INDI-derived rows by §2.4.

Rules for every class: Wyss DBS Localizer never bundles, mirrors or proxies connectome files; each user's machine (or the user's own server) downloads from the original host. The server never serves connectome files to clients (§4.8). Citations and the acknowledgement wording the terms require are shown in the GUI card, written to `<root>/licenses/<id>.json` and to every output's provenance sidecar (§9.3).

### 2.4 Sources and access after the open-source review (amendment 2026-11-10; normative, overrides §2.2 / §2.3 where they differ)

Checked on 2026-11-10 against the owners' public pages (no archive content was inspected):

| Id | Other published source | Same file as the sciebo one? | Decision |
|----|------------------------|------------------------------|----------|
| `elias2024-dtor985-quarter` / `-half` / `-full` | figshare, the authors' own records (collection doi:10.6084/m9.figshare.c.6844890; articles 24172989, 24172995, 24172992, version 2, **CC0 1.0**), one file each, MD5 published | Same names and byte counts; digests in the table below | **Source changes to figshare** (`Source::Figshare`, below). The sciebo copy is no longer used. |
| `loh2020-torpd75` | Zenodo record 4310183 (CC BY 4.0): `Tor-PD_connectome.zip`, 12 831 318 183 bytes | No, a different package (about half the size; content not compared) | Stays on the lead-dbs.org source. |
| `horn2016-nkirs169` | NITRC project `gibbsconnectome` (posted by the connectome's author, licence field "GNU GPL v2"): 364 files, 79 GB, per-subject matrices and multi-part `.rar` archives | No, a different package | Stays on the lead-dbs.org source, access `open` with the notice *for non-commercial research use*. Reason: the source cohort's neuroimaging release "does not require a data usage agreement" (INDI enhanced NKI-RS access page) and is on a public S3 bucket; the author published the same cohort's connectome on NITRC without access control. The licence itself stays unstated on lead-dbs.org. |
| `alfatly2023-pediatric100` | None. The paper (Al-Fatly 2023, NeuroImage: Clinical, CC BY) states the connectome "can be downloaded and queried from the Lead-DBS interface" | — | Stays on the lead-dbs.org source, access `open` with the notice *for non-commercial research use; source cohort CoRR (INDI)*. The paper names the cohort (CoRR sub-cohort nyu2, 107 subjects aged 6 to 18, 100 after motion exclusion); the CoRR page that describes NYU 2 as "187 neurotypical adults" contradicts it and is not resolved. No licence is stated anywhere. |
| `horn2013-gibbs19` | None; the Data in Brief article points to lead-dbs.org | — | Unchanged (`open`). |
| `horn2017-mghusc-hcp32`, `ewert2017-ppmi85`, `horn2017-ppmi74p15c` | None. lead-dbs.org gives the download link without access steps | — | **Import-only** (`dua`, §2.3): no download by Wyss DBS Localizer. |
| `cohen2023-gsp1000v2` | Harvard Dataverse is the owner's host | — | Unchanged. |

lead-dbs.org states no licence or terms for the hosted packages, documents no other download address than `download.php?id=<id>` and publishes no checksums; its `robots.txt` does not disallow `download.php`. On 2026-11-11 the Lead-DBS maintainers stated by e-mail that the data packages they host, connectomes and atlases included, carry no licence, and that they do not explicitly forbid their use by other software (`resources/licenses/connectomes/leaddbs-hosting.txt`). The owner reads this as covering downloads by Wyss DBS Localizer through the host.

**`Source::Figshare { file_id }`**: `GET https://ndownloader.figshare.com/files/<file_id>` and follow the redirect to the storage URL (re-requested when a signed URL expires), `Range` resume and all rules of §4.4 as for Dataverse; `source_digest` = `md5:<hex>` as published by the figshare API; the base URL is a `FetchSettings` field so tests use a local server.

| Id | figshare file id | File | Bytes | MD5 (figshare) | SHA-256 (ours) |
|----|-----------------:|------|------:|----------------|----------------|
| `elias2024-dtor985-quarter` | 52596914 | `dTOR-985_Quarter.zip` | 2 886 958 128 | `c75d3044f82ab68d1268263f8e7ff7c0` | `dfac8ca9673707ffd396b4a8ef48028a79041610711d8b569e3308967c4cb34a` |
| `elias2024-dtor985-half` | 52597346 | `dTOR-985_Half.zip` | 5 776 636 849 | `97ca0b18ea4fbfeb27192964efdcb1f5` | `d50ca0896b12842b6176905e7e7f19aeeffc7eb999d504c0d2e46b7120aea986` |
| `elias2024-dtor985-full` | 52597988 | `dTOR-985_Full.zip` | 11 551 215 059 | `576841ad68eb01e8ed2c9b4a59608fbf` | `c244c127d0e9d98926586259d9cce74e89d0d57760494751f50e61f60765a132` |

The three figshare files were streamed and hashed on 2026-11-10: their MD5 equals the figshare value and their SHA-1 equals the sciebo `OC-Checksum` of §2.2, so figshare and sciebo serve the same bytes.

For these three entries `dataset_url` is the figshare collection `https://doi.org/10.6084/m9.figshare.c.6844890`.

**Steps of the import-only entries** (catalogue field `steps`, shown as a numbered list; each step is a sentence plus an optional link). The archive names are the entry's `files[].file_name`.

`ewert2017-ppmi85` and `horn2017-ppmi74p15c`:

1. Read the PPMI Data Use Agreement. Link: `https://www.ppmi-info.org/sites/default/files/docs/ppmi-data-use-agreement.pdf`
2. Create an account at the LONI Image and Data Archive and submit the PPMI application. PPMI states that applications are reviewed within one week. Link: `https://ida.loni.usc.edu/collaboration/access/appApply.jsp?project=PPMI`
3. After PPMI approved your application, download the archive `<file name>` in your web browser from the Lead-DBS *Normative connectomes* page. Link: `https://www.lead-dbs.org/helpsupport/knowledge-base/atlasesresources/normative-connectomes/`
4. Tick both boxes below, choose *Import archive…* and pick the downloaded file. If Lead-DBS already installed this connectome on this computer, choose *Choose folder…* and pick its folder instead.
5. Follow the PPMI Publication Policy and use the acknowledgement shown below in publications. Link: `https://www.ppmi-info.org/sites/default/files/docs/ppmi-publication-policy.pdf`

`horn2017-mghusc-hcp32`:

1. Read the HCP Data Use Agreement of the MGH-USC Adult Diffusion data. Link: `https://www.humanconnectome.org/storage/app/media/documentation/mgh_diffusion/HCP_Data_Agreement.pdf`
2. Create an account at the LONI Image and Data Archive, submit the HCP application and accept the agreement there. Link: `https://ida.loni.usc.edu/collaboration/access/appApply.jsp?project=HCP`
3. After your access is confirmed, download the archive `<file name>` in your web browser from the Lead-DBS *Normative connectomes* page. Link: `https://www.lead-dbs.org/helpsupport/knowledge-base/atlasesresources/normative-connectomes/`
4. Tick both boxes below, choose *Import archive…* and pick the downloaded file. If Lead-DBS already installed this connectome on this computer, choose *Choose folder…* and pick its folder instead.
5. Use the acknowledgement shown below in publications.

Entries of the other classes have an empty `steps` list and keep their one-paragraph `instructions`.

---

## 3. Folder layout and file formats

### 3.1 Connectome roots

- **Primary root** (writable): `<user data dir>/connectomes/` (`template_catalog::user_data_dir`, i.e. `%APPDATA%\dbsloc`, `~/Library/Application Support/dbsloc`, `$XDG_DATA_HOME/dbsloc`; `DBSLOC_DATA_DIR` moves it), overridden by `DBSLOC_CONNECTOMES_DIR` or the GUI setting *Connectomes folder*. Downloads and imports of archives go here.
- **External Lead-DBS root** (read-only, optional): `DBSLOC_LEADDBS_CONNECTOMES_DIR` or the GUI setting *Lead-DBS connectomes folder* — an existing Lead-DBS `connectomes/` folder (default location `<Lead-DBS>/connectomes/`, or wherever the user's `prefs.lc.datadir` points). It is read in place, never copied and never written (same rule as `DBSLOC_LEADDBS_ATLASES_DIR`, `specs/11` §1). Licence records for its connectomes go to the primary root, caches to the cache dir (§3.5).
- **Registered folders**: single connectome folders imported in place (§4.3), listed in `<primary>/registry.json` (`[{ "path", "class", "id", "registered_at" }]`).
- Discovery: for each root, every non-hidden subfolder of `dMRI/` containing `data.mat` (or, our extension, `data.trk` / `data.trk.gz`) is a structural connectome; of `fMRI/` containing `dataset_info.json` a functional one (each subset is a selectable item, as the reference lists `<name> > <subset>`); of `dMRI_MultiTract/` containing at least one `*.mat` a multi-tract one. Folder name = display name. A name present in more than one root: primary root wins, then registered, then external; the GUI shows the duplicate with its path.
- Catalogue match: an installed folder matches a catalogue entry when its folder name equals the entry's folder (or `dataset_info.json` `tag` equals the entry's tag).

### 3.2 Structural `data.mat` (fibre format, `ea_fibformat` 1.0 / 1.1)

Variables (MATLAB names; row × column as MATLAB sees them):

| Variable | Shape / class | Meaning |
|----------|---------------|---------|
| `fibers` | P × 4, `single` or `double` | All points of all fibres stacked fibre after fibre: columns x, y, z, fibre number (1-based, equal for all points of one fibre, 1…K in order). |
| `idx` | K × 1 (also 1 × K seen in user files), numeric | Number of points of each fibre; Σ idx = P. Fibre f occupies rows `o_f … o_f + idx_f − 1`, `o_f = 1 + Σ_{g<f} idx_g`. |
| `ea_fibformat` | char, `'1.0'` or `'1.1'` | Format version. A file without it is a legacy format (§3.2.3). |
| `fourindex` | scalar 1 | Marks the four-column layout. |
| `voxmm` | char `'mm'` or `'vox'` | Units of columns 1–3. Missing: `'mm'` when any coordinate is negative, else `'vox'` (reference inference). |
| `mat` | 4 × 4 double, optional | With `'vox'`: maps **1-based** voxel coordinates to mm (SPM convention): `p_mm = mat · [p_vox; 1]`. |
| `vals` | K × 1, optional | Per-fibre value (default 1). Read and kept; not used by §5–§7. |

Verified on `MGH-USC HCP 32 (Horn 2017)/data.mat` (read from the archive's first 64 KB, 2026-10-23): **MAT v7.3** (HDF5 with the 512-byte MATLAB user block, created on MACI64, 2020-01-29); variables `ea_fibformat` (char, 3 × 1 in HDF5), `fibers`, `fourindex`, `idx`, `vals`, `voxmm`; `fibers` is an HDF5 dataset of dataspace **(4, 396 595 508)**, IEEE float32 — i.e. MATLAB P × 4 single with **P = 396.6 million points** (6.3 GB uncompressed). Whether the other downloadable structural connectomes are v7.3 or v5/v7 is **unverified**; both must be read.

#### 3.2.1 MAT container rules

- **MAT v7.3** (HDF5): MATLAB is column-major, HDF5 row-major, so a MATLAB r × c array is an HDF5 dataset of dataspace (c, r); element (i, j) of the MATLAB array is HDF5 element [j][i]. `fibers` therefore arrives as 4 rows of P values: row 0 = all x, row 1 = all y, row 2 = all z, row 3 = all fibre numbers. Chunked + deflate datasets must be read chunk by chunk (hyperslab of a column range), never as one allocation. `fastwarp/crates/fastwarp-matfile` (`matv73`) reads the containers; it must gain a streaming slice read of a dataset range (implementation note).
- **MAT v5 / v7** (`read_mat5`): `miMATRIX`, optionally wrapped in `miCOMPRESSED` (one zlib stream per variable, so a compressed variable is inflated sequentially). Data are stored column-major: the first P values are x, then P values y, then z, then the fibre numbers. A v7 variable is limited to 2³¹ bytes, so big connectomes are v7.3.
- Element classes: `single` → f32, `double` → f64 converted to f32 for points (fibre numbers and `idx` read as f64 and checked to be integers ≤ 2³²).

#### 3.2.2 Validation (reader)

`E_CONN_FORMAT` when: `fibers` missing or not 4 columns (3 columns accepted only when `idx` is present: fibre numbers are then derived); `idx` missing; Σ idx ≠ P; any idx ≤ 0; a fibre-number column that is not constant within each fibre's run or not 1…K in order (checked while streaming); `voxmm = 'vox'` without `mat` and without a reference image supplied by the user; non-finite coordinates. Fibre numbers are never trusted for indexing: the reader derives fibre membership from `idx` (prefix sums) and only checks column 4.

#### 3.2.3 Legacy and other structural formats

- Files without `ea_fibformat` (older layouts: a cell array of per-fibre point arrays under names such as `normalized_fibers_mm`, `curveSegCell`, `fibs`; Freiburg voxel layouts with an axis swap): **not read in v1**; the reader reports `E_CONN_FORMAT` "legacy fibre format; convert it with Lead-DBS or re-export". (The reference converts these on first load and overwrites the file — Wyss DBS Localizer never rewrites a connectome.)
- **TrackVis `.trk` / `.trk.gz`** (our extension for `data.trk`, and import of user tractograms): header of exactly 1000 bytes, little-endian (if `hdr_size` reads 1000 only after byte-swapping, the file is big-endian): `id_string` char[6] = `"TRACK\0"`, `dim` i16[3], `voxel_size` f32[3], `origin` f32[3], `n_scalars` i16, `scalar_name` char[10][20], `n_properties` i16, `property_name` char[10][20], `vox_to_ras` f32[4][4] (row-major), `reserved` char[444], `voxel_order` char[4], `pad2` char[4], `image_orientation_patient` f32[6], `pad1` char[2], `invert_x/y/z`, `swap_xy/yz/zx` u8 each, `n_count` i32 (0 = unknown, read to end of file), `version` i32, `hdr_size` i32 = 1000. Each track: i32 `n_points`, then `n_points` × (3 + n_scalars) f32, then `n_properties` f32. Point coordinates are in "voxmm": millimetres from the corner of voxel (0, 0, 0) along the voxel axes. World mm: `p_vox = p_voxmm / voxel_size − 0.5` (voxel-centre convention), `p_world = vox_to_ras · [p_vox; 1]`. A `vox_to_ras` of all zeros (version 1 files) requires a reference NIfTI (its affine, 0-based) — else `E_CONN_FORMAT`. Note: the reference's converter (`ea_trk2ftr`) applies `vox_to_ras` to `p_voxmm / voxel_size` **without** the −0.5 shift and additionally flips axes when `pad2` disagrees with the affine's signs; parity tests against reference-converted files must allow a half-voxel offset (whether the omission is intentional is **unverified**).
- **DSI Studio `.fib.gz`**: not supported (requires on-demand tracking).

### 3.3 Functional connectome folder (`fMRI_timecourses`)

```
fMRI/<name>/
  dataset_info.json        name, tag, type, subsets, citation, [formatversion]
  dataset_info.mat         (MATLAB copy of the JSON; ignored)
  dataset_volsurf.mat      vol.outidx, vol.space, vol.subIDs, [surf.l, surf.r]
  vol/<run file>.mat       one file per subject and run, variable gmtc
  surf/{lh.,rh.}<run>.mat  optional surface time courses (ignored in v1)
  [GSP1000.txt]            GSP v2 only: list of run files
```

**`dataset_info.json`** (verified for PPMI 74/15 and GSP 1000 v2; the HCP 32 structural one has the same `name` / `tag` / `citation` keys):

| Key | Type | Meaning |
|-----|------|---------|
| `name` | string | Display name, equals the folder name. |
| `tag` | string | Label for output names (`GSP1000v2`, `PPMI74P15C`, `MGHUSCHCP32`). |
| `type` | `"fMRI_timecourses"` \| `"fMRI_matrix"` | Only the first is supported in v1. |
| `subsets` | array of `{ "name": string, "subs": [int, …] }` | 1-based indices into `vol.subIDs`. |
| `citation` | `{ "name"?, "short", "long": string \| [string] }` | Shown in the GUI and sidecars. |
| `formatversion` | number, optional (1.0 when absent) | 1.0: `gmtc` is N_vox × T; 1.1: T × N_vox. **Absent in both files inspected**. |

Encoding: the file inspected for HCP 32 is **not valid UTF-8** (Windows-1252 en dash and umlaut bytes in `citation`). The reader decodes UTF-8 and falls back to Windows-1252 on an invalid sequence.

**`dataset_volsurf.mat`** (verified on GSP 1000 v2: MAT v7.3, 2.2 MB):

| Field | Shape / class | Meaning |
|-------|---------------|---------|
| `vol.outidx` | 1 × N double (GSP: N = 285 903) | 1-based MATLAB linear indices (column-major) of the brain voxels in `vol.space`. **Not sorted** (GSP starts 407458, 407459, 407460, 397537, …): the column / row order of every `gmtc` follows this order. Linear index L → 0-based voxel (i, j, k): `i = (L−1) mod n_x`, `j = ⌊(L−1)/n_x⌋ mod n_y`, `k = ⌊(L−1)/(n_x n_y)⌋`. Values must be unique and within the grid (GSP: 33 257 … 779 376, unique). |
| `vol.space` | struct: `dim` 3 × 1, `mat` 4 × 4, `dt`, `fname` (`222.nii`), `img`, … | Grid: GSP `dim` = 91 × 109 × 91, `mat` = 2 mm diagonal with translation (−92, −128, −74), a **1-based** SPM matrix, i.e. 0-based origin (−90, −126, −72) — identical to the reference 2 mm grid `222.nii` (§5.4). |
| `vol.subIDs` | cell 1000 × 1 of cells `{id, run1, run2, …}` | Subject j's id string and its run file names (relative to `vol/`); e.g. `{'0001','sub-0001_run_1.mat','sub-0001_run_2.mat'}`, `{'0002','sub-0002_run_1.mat'}`. Number of runs = cell length − 1. |
| `surf.l`, `surf.r` | optional | fsaverage5 hemispheres (10 242 vertices); ignored in v1. |

**Run file `vol/<run>.mat`** (verified on GSP `sub-0001_run_1.mat`, from the tar header): **MAT v5 with `miCOMPRESSED`** (125 491 001 bytes compressed, 137 233 488 inflated), one variable `gmtc`, class `single`, dims **285 903 × 120** (N_vox × T, i.e. the 1.0 orientation). PPMI 74/15 run files are 83–85 MB inside the zip; container and orientation **unverified**. Orientation rule (reader): the dimension equal to N = `numel(vol.outidx)` is the voxel axis; when both dimensions equal N, use `formatversion`. Column/row v of `gmtc` belongs to voxel `outidx[v]`. The repetition time is not stored and is not needed for seed maps.

**GSP 1000 v2 install layout:** the 35 tars contain flat members `GSP1000.txt` and `sub-XXXX_run_N.mat` (ustar, no folder prefix; verified for tar 00); the installer writes run files to `fMRI/GSP 1000 v2 (Cohen 2023)/vol/`, `GSP1000.txt` to the folder top, and the three small Dataverse files (`dataset_info.json`, `dataset_info.mat`, `dataset_volsurf.mat`) to the folder top. Member names other than `GSP1000.txt` and `sub-[0-9]{4}_run_[0-9]+\.mat` are refused.

### 3.4 Multi-tract connectome (`dMRI_MultiTract/<name>/`)

A folder of `*.mat` files, each in the §3.2 format (one pathway per file), excluding `*_ADJ.mat` (adjacency matrices, not used). Merged view: files in lexicographic order of their names (the reference uses directory order; **unverified** whether that equals name order on every platform — we fix name order); fibre numbers renumbered globally (`global = local + offset_p`, `offset_p` = fibres of all earlier files); per-pathway `start` index and display name (file stem with `_` → space) kept; a per-file scalar `mirrored` (1 = the pathway set is left–right symmetric) is kept, and the merged set is `mirrored` only when every file says so. The reference writes the merged set into the group folder as `merged_pathways.mat`; we merge in memory (or in the cache, §3.5) and never write into the connectome folder. Which Lead-DBS downloads populate `dMRI_MultiTract` is **unverified** (none of the installer entries does).

### 3.5 Our canonical representation and optional cache

The readers above are the source of truth: no conversion step is required to use a connectome. For speed, an optional per-connectome cache is built on first use (amended 2026-10-25 after measurements on Horn 2013 Gibbs 19 Full, see the end of this section):

```
<cache dir>/connectomes/<connectome key>/v1/
  meta.json        { source_path, source_size, source_mtime, source_probe_sha256, format, P, K,
                     bbox_mm, space, built_by, reader_version, cache_version: 1,
                     order: "morton-mid-2mm", cell_mm: 4, grid_origin_mm, grid_dims }
  points.f32       P × 3 little-endian f32, x y z per point, fibre after fibre in cache order
  offsets.u64      K + 1 little-endian u64 prefix sums in cache order (offsets[0] = 0, offsets[K] = P)
  order.u32        K little-endian u32: cache fibre id → 0-based source fibre id
  vals.f32         K little-endian f32 in cache order
  cells.u64 / cells.u32   spatial index (below)
```

- **Cache dir** (amended 2026-10-25): the operating system's local, non-roaming cache folder, `dbsloc` subfolder — `%LOCALAPPDATA%\dbsloc\cache` on Windows, `~/Library/Caches/dbsloc` on macOS, `$XDG_CACHE_HOME/dbsloc` (default `~/.cache/dbsloc`) on Linux; `DBSLOC_CACHE_DIR` overrides it. Gigabytes of rebuildable data must not roam with a Windows profile. Caches of every root, including the read-only external Lead-DBS root, go here.
- `connectome key` = class + "-" + folder name with every character outside `[A-Za-z0-9._-]` replaced by `_`, + "-" + the first 8 hex digits of SHA-256 of the canonical source path.
- `source_probe_sha256` = SHA-256 of (source size as 8 bytes LE ‖ first 16 MiB ‖ last 16 MiB). The cache is valid only when size, mtime, probe hash, `reader_version` and `cache_version` match; otherwise it is rebuilt. `reader_version` is a constant of the reader crate, raised by any reader fix that can change the points read.
- **Fibre order:** fibres are stored in ascending Morton (Z-order) code of their middle point (point `⌊n/2⌋` of the fibre, 0-based), quantised to 2 mm cells from `floor(bbox_lo / 2) · 2`, 10 bits per axis (clamped), ties by ascending source id. Fibres near each other in space are then near each other on disk, so a region query reads a small contiguous part of `points.f32`. `order.u32` maps back; **every output that names a fibre (§7.3 feature ids, §7.4 `source_fiber_index`, subset files) uses source ids**, so results are identical with and without the cache.
- **Spatial index:** a dense grid of 4 mm cells with origin `grid_origin_mm = floor(bbox_lo / 4) · 4` and `grid_dims = floor((bbox_hi − origin) / 4) + 1` per axis (cell (i, j, k) covers `[origin + 4·(i, j, k), origin + 4·(i+1, j+1, k+1))`, linear id `i + n_x (j + n_y k)`); `cells.u64` = (n_cells + 1) offsets, `cells.u32` = for each cell the ascending cache fibre ids of fibres with at least one point in it. Query = all fibres of the cells overlapping an axis-aligned box, then the exact §5.2 test on their points.
- Built in `.partial-<key>-<pid>-<n>/` and renamed; read either whole (`FibreSet` in source order) or memory-mapped for region queries (a subset `FibreSet` plus its source ids). On Windows a mapped file cannot be deleted: `cache clear` reports a cache still in use and leaves it, never fails half-way.
- Size ≈ 14.4 bytes per point (12 points + 4 · 0.59 (fibre, cell) pairs per point measured on Gibbs 19 Full, + offsets / order / vals): Gibbs 19 Full (P = 70.4 M) ≈ 1.04 GB; HCP 32 (P = 396.6 M) ≈ 5.7 GB (**estimated**).
- **Consent (owner decision 2026-10-25).** A cache uses gigabytes of disk, so it is never built without the user's consent for that connectome. The choice is stored per connectome key in `<cache dir>/connectomes/choices.json` (`{ "<key>": { "choice": "allow" | "decline", "decided_at", "estimated_bytes" } }`, written atomically).
  - Estimated size before building = `14.4 × P` bytes (P from the reader's header probe without inflating; for a multi-tract set the sum) and is what the user is shown.
  - **GUI:** the first time a structural connectome is used by a job (stimulation-editor *Compute connectivity*, a group fibre filtering / network mapping run) and no choice is recorded, a dialog asks before the job starts: "*Speed up <name>?* Keep a ready-to-use copy on this computer: about <size> of extra disk space (<free> free). Later jobs with this connectome then start in well under a second instead of about <n> s." with *Keep a copy* / *Not now* and a tick box *Don't ask again for this connectome* (ticked by default). *Keep a copy* builds the cache, records `allow` once the build succeeded (a cancelled or failed build records nothing, so the next use asks again), then runs the job; *Not now* runs the job with the plain reader (recording `decline` only when the tick box is set). The dialog also shows when free space after the build would be below 5 GB, with *Keep a copy* disabled and the reason.
  - **CLI / server / runner** never ask. With no recorded choice they use the plain reader and print once per job `dbsloc: note: connectome cache for <name> not built (about <size>); build it with 'dbsloc connectome cache build <id>'`. `cache build` records `allow` after a successful build.
  - Setting *Connectome cache* (`DBSLOC_CONNECTOME_CACHE`): `ask` (default: the rules above), `always` (build without asking when free space ≥ 2 × the estimated size + 5 GB), `never` (never build or read a cache).
  - An allowed connectome whose cache is missing or stale (validity rule above) is rebuilt without asking again, provided the free-space rule holds; otherwise the plain reader is used with a warning.
- **Details (2026-10-25, from the implementation):** under `ask`, a valid cache that already exists is used even when no choice is recorded (it can only exist because someone built it); a source without `vals` gets an empty `vals.f32` and `has_vals: false` in `meta.json`, so a whole-set load returns exactly what the plain reader returns; `choices.json` entries also carry the connectome `name` for the declined list; multi-tract connectomes are not cached in v1.
- **Clearing.** `dbsloc connectome cache list|build|clear <id>|--all`; `clear` removes the cache folder and resets the choice to "not decided" (the next use asks again). GUI: the *Connectomes…* window has *Manage cache…*, which lists every cache under the cache dir (connectome name, size on disk, last used = modification time of `v1/last_used`, touched on each use, source path; an orphaned cache whose source is gone is listed as such) with a tick box per row, *Select all* / *Select none*, the total size of the ticked rows and *Delete selected…* (confirmation naming the count and size; a cache in use by a running job or still mapped is skipped and reported). Below, connectomes with a recorded *decline* are listed with *Ask again*.
- Functional connectomes are not cached in v1 (the run files are compressed MAT v5; a raw f32 copy of GSP would double 219 GB). Implementations may keep per-run column means and standard deviations in memory during one pass.
- **Reading the source** (amended 2026-10-25): chunked + deflate HDF5 datasets are inflated in parallel (several chunks read ahead, inflated on a worker pool, delivered in order), and the fibre index is built in parallel; the result is bit-identical to a sequential read.
- **Measurements (informative, 2026-10-25, Intel Core Ultra 7 268V, NVMe, Gibbs 19 Full `data.mat` 1.65 GB, `fibers` stored as double in 4 × 2047 chunks, deflate 3, P = 70 354 484, K = 2 375 758):** sequential read + inflate 6.6–6.9 s plus index 2.9–4.0 s, peak commit 2.55 GB (CPU-bound; the file itself reads in 0.5–0.7 s); parallel inflate 1.9–2.2 s, 0.85 GB; cache build ≈ 4.5 s; whole cache read 0.32 s + index 0.10 s; region query of a 12 mm VTA box 0.075 s warm / 0.48 s cold, ≈ 2–3 % of the fibres.

---

## 4. Download, licence and import flow

### 4.1 Catalogue entry (`crates/dbsloc-pipeline/src/connectome_catalog.rs`, the only place naming connectome sources)

| Field | Type | Notes |
|-------|------|-------|
| `id` | `&str` | Table §2.2. |
| `name` | `&str` | Lead-DBS display / folder name. |
| `class` | `dMRI` \| `fMRI` \| `dMRI_MultiTract` | Install subfolder. |
| `space` | template space id | `MNI152NLin2009bAsym` except the pediatric set. |
| `cohort`, `citation`, `acknowledgement` | text | Shown in the card; written to records. |
| `licence_name`, `licence_url`, `licence_text` | text | `licence_text` is the exact text shown and hashed (terms verbatim for `cohort_terms` / `dua`). |
| `access` | `open` \| `cohort_terms` \| `dua` | §2.3. |
| `dataset_url` | URL | The cohort's website (link in the card). |
| `instructions` | text | What the user must do (register, accept, download, where to put the folder). |
| `steps` | list of `{ text, url? }` | Numbered instructions of an import-only (`dua`) entry (§2.4); empty for the other classes. |
| `files` | list of `{ source, file_name, bytes, source_digest, sha256 }` | `source` = `LeadDbs { id }` (resolved via `download.php`), `Dataverse { file_id }` or `Figshare { file_id }` (§2.4); `source_digest` = `sha1:<hex>` (sciebo) or `md5:<hex>` (Dataverse, figshare); `sha256` = empty until pinned (§4.5). For a `dua` entry `files` names the archive the user obtains; it is used to verify an imported archive (§4.3), never to download. |
| `install` | `ZipFolder { folder }` \| `GspTars { folder }` | How files become the folder. |
| `marker` | relative path | File whose presence means installed (`data.mat`, `dataset_info.json`). |
| `version` | text | Shown; written to the record. |

### 4.2 GUI card (Settings → *Connectomes*, also reachable from the group / stimulation panels)

One row per catalogue entry plus every discovered folder that matches no entry. Row: name, class (*Structural* / *Functional*), cohort, size, state (*Installed* with path and version · *Not installed* · *Partial download (NN %)* · *Requires your data access*). Expanding a row shows: citation; acknowledgement wording; licence name and a scrollable box with the licence text (or terms verbatim); *Open dataset website* link; free space on the target volume and the space needed; then, by access class:

- `open` / `cohort_terms`: tick *I have read and accept these terms* (unticked by default; required) → *Download (NN GB)*.
- `dua` (import-only, amended 2026-11-10): **no *Download* button.** The card shows the state *Import only: needs your own data access*, the section *How to get this dataset* open by default with the entry's `steps` as a numbered list (each link clickable, opened in the web browser), then the two ticks *I have read and accept these terms* and *I hold an approved data use agreement with <cohort> that covers this use*. *Import archive…* and *Choose folder…* are the primary actions, in the card's button row where *Download* is for the other classes. Both buttons work without the ticks (the folder is then usable without a licence record, §4.3); the hint under the ticks says so.

- **Installed without a licence record (2026-11-10):** for an installed `cohort_terms` or `dua` entry (primary, registered or external root) whose record is missing or stale (§4.7), the card shows *Licence and terms* open by default with the terms text, *I have read and accept these terms*, for `dua` also the attestation tick, and *Record acceptance*, enabled only when the ticks required for the class are set (disabled with the reason as hover text otherwise). Clicking it writes `<primary>/licenses/<id>.json` with `accepted_via: "gui"` (attestation sentence for `dua`, `null` for `cohort_terms`; an earlier record is archived as in §4.7), refreshes the list and says *Recorded your acceptance of the terms of <name>.* A write error is shown as the other panel errors. No network access, and no other file is written. An `open` entry needs no record and shows no section once installed.

During a download: progress bar (bytes, rate, remaining), *Cancel* (keeps the partial file for resume, §4.4), and for GSP the file counter (n / 38). Errors show one line with the message of §11. *Choose folder…* registers a folder in place (§4.3). *Remove…* deletes an installed folder of the primary root only (confirmation; never offered for the external root or registered folders, which are *Forget* instead). Closing the application cancels a running download (partial kept).

### 4.3 CLI

```
dbsloc connectome list [--json]                      catalogue + discovered folders, state, size, path
dbsloc connectome info <id|path> [--remote]          local facts (format, P, K, subsets, tag, licence record);
                                                     --remote: one HEAD (or Dataverse metadata GET) to compare
                                                     size / digest with the pin
dbsloc connectome fetch <id> --accept-terms [--attest-data-use-agreement] [--root DIR]
dbsloc connectome import <folder|zip|tar…> [--class dMRI|fMRI|dMRI_MultiTract]
dbsloc connectome forget <path>
dbsloc connectome cache build|clear <id|path>
dbsloc connectome map …  /  dbsloc connectome fibervals …     (§9)
```

- `fetch` without `--accept-terms` prints the licence text and exits 2 ("pass --accept-terms to accept these terms"); with an interactive terminal it may instead ask the user to type `yes`. A `dua` entry also needs `--attest-data-use-agreement`. Exit codes: 0 done, 1 error, 2 usage / terms not accepted, 130 Ctrl-C (partial kept).
- **`fetch` of a `dua` entry (amended 2026-11-10)** never contacts the network: it prints the citation, the numbered `steps` with their links and the hint `dbsloc connectome import <archive> --accept-terms --attest-data-use-agreement`, and exits 2 with `E_CONN_TERMS` "<id> is import-only: obtain it with your own data access and import it". The flags `--accept-terms` / `--attest-data-use-agreement` do not change that. The server's fetch endpoint (§4.8) answers the same refusal.
- **Verification of an imported archive (2026-11-10):** when the base name of an archive given to `import` equals the `file_name` of a catalogue file (any access class) ignoring ASCII case, or is that name with a browser duplicate counter (the catalogue stem, an optional space, `(` digits `)`, then the same extension: `name (1).zip`, `name(2).zip`), its size and `source_digest` (and `sha256` when pinned) must equal the pins before anything is extracted; a mismatch is `E_CONN_CHECKSUM` and nothing is written. The message reads "<path> is named like the catalogue file <file_name> of <entry name> but is not that file: <N bytes instead of M | SHA-1 … instead of …>. If the download was interrupted, download it again. If this is your own archive, rename it and import it again; it will then be imported without verification." An archive with another name is imported as before, unverified, and the outcome says so ("not a known catalogue file; not verified").
- `import <folder>`: validates the folder (§3.2–§3.4), records it in `registry.json`; never copies. `import <zip|tar…>`: extracts into the primary root (zip / tar entries with absolute paths, `..` components, symlinks or device files are refused, `E_CONN_ARCHIVE`), then validates. If the folder matches a catalogue entry with `access = dua` / `cohort_terms` and no licence record exists, `import` writes a record with `accepted_via: "import"` only after the same acceptance flags as `fetch`; without them the folder is usable but every output sidecar notes `licence_record: null`.

### 4.4 Polite download behaviour (normative)

1. **One transfer at a time** per connectome root and per process: a lock file `<primary>/.download.lock` (`{pid, host, started_at, id}`) taken atomically; a lock whose pid is not running on this host is stale and replaced. A second request queues in the GUI (*Waiting for the current download*) and fails in the CLI (`E_CONN_BUSY`). Files of a multi-file entry (GSP) are fetched sequentially.
2. **No parallel segments**, no multi-connection acceleration, no prefetching, no polling. The catalogue is compiled in; listing never touches the network. `--remote` is the only metadata request and is user-initiated.
3. **Resolution:** for `LeadDbs` sources, each fetch (and each resume after a restart) starts with `GET https://www.lead-dbs.org/release/download.php?id=<id>` without following redirects, reads `Location`, and downloads from it; the resolved URL is reused for retries within the same invocation and re-resolved once if it answers 404/410. The host of `Location` is not hard-coded (the Horn lab may move storage). Dataverse: `GET /api/access/datafile/<id>` → 303 → the signed URL (re-requested when it expires).
4. **User-Agent:** `dbsloc/<version> (Wyss DBS Localizer; research software)` on every request (`version.rs`). No cookies are stored across runs; no user or machine identifiers are sent.
5. **Resume:** data stream to `<primary>/<class>/.partial-<id>/<file_name>.part` with a sidecar `<file_name>.part.json` (`{url, etag, last_modified, bytes, source_digest}`). On resume, `Range: bytes=<n>-` with `If-Range: <etag>`; `206` with a matching `Content-Range` continues; `200` (Range ignored or the file changed) restarts from byte 0 after truncation; `416` restarts from 0. Hashes are re-computed over the existing partial from disk before continuing (no network). A partial older than 30 days is removed on the next fetch of any entry.
6. **Retries:** at most 3 attempts per file per invocation, after 60 s and 300 s (or the server's `Retry-After` when larger, capped at 1 h); retried only on connection errors, timeouts, 408, 429, 500, 502, 503, 504; any other 4xx stops at once. Every retry resumes (rule 5). Timeouts: the `template_fetch.rs` agent settings (connect / response timeouts; the body timeout scales with the pinned size).
7. **Bandwidth:** unthrottled by default; `--max-rate <MB/s>` and a GUI setting cap it.
8. **Body limit:** the pinned byte count; a longer body stops with `E_CONN_SIZE`.

### 4.5 Verification and pins

- Pins per file: `bytes` and `source_digest` (the host's published SHA-1 or MD5, §2.2 / Appendix A). While streaming, the downloader computes SHA-1 or MD5 (whichever the pin needs) **and** SHA-256. If a response carries `OC-Checksum` that differs from the pin, the transfer stops before data is written (`E_CONN_CHANGED` "the server copy of <id> changed; update Wyss DBS Localizer"); the same when `Content-Length` (or the total in `Content-Range`) differs from `bytes`.
- After the transfer: size and `source_digest` must equal the pins, else `E_CONN_CHECKSUM` (both values in the message) and the partial is deleted.
- **Our own SHA-256 after the first verified download:** the computed SHA-256 is written into the install record (`<folder>/.dbsloc-install.json`) and printed (`--json` field `sha256`). A maintainer copies verified values into the catalogue's `sha256` field in the next release; from then on a mismatch against `sha256` is also `E_CONN_CHECKSUM`. lead-dbs.org itself publishes no checksums; its storage host's SHA-1 is the published digest we rely on.
- Archives are extracted only after verification.
- **Pinned SHA-256 values (2026-11-10).** `horn2013-gibbs19` `groupconnectome_horn_2013.zip`: `0895deb030b6e9cc2a15e71fa7f021507cd1494586fa99bba6f9f60fb573962e` (from the install record of a verified download, 2026-10-24). The three dTOR-985 files: the values in the catalogue, computed on 2026-11-10 by streaming the figshare files (MD5 equal to the figshare value). **2026-11-11:** `horn2016-nkirs169`, `loh2020-torpd75`, `alfatly2023-pediatric100`, and of `cohen2023-gsp1000v2` the three small files plus `GSP1000_leadDBS_00.tar` to `_14.tar`; **2026-11-12:** `GSP1000_leadDBS_15.tar` to `_34.tar`. All are the values in the catalogue, computed by streaming each file from its host without writing it to disk; every file's size and host digest (SHA-1 for lead-dbs.org, MD5 for Dataverse) equalled the pins. Every downloadable file is now pinned; only the `dua` files stay unpinned (`sha256` empty) until a maintainer has downloaded them; a `dua` file can only be pinned by a maintainer who holds that agreement.

### 4.6 Install

- Free space check before the first byte: for zips, `bytes` (archive) + Σ uncompressed member sizes (from the zip's central directory once downloaded; before that, 1.02 × `bytes`) + 1 GB; for GSP, Σ `bytes` + the largest tar + 1 GB. Failure: `E_CONN_DISK` "not enough free disk space for <id>: need <N> GB, <M> GB free".
- Zip: extract into `<primary>/<class>/.partial-<id>-<pid>-<n>/` (zip-slip rules of §4.3), then choose the entry's folder among the top-level entries: the one equal to the entry's `folder`; when no top-level folder has that name and the folder is **unverified**, the single top-level folder (its name is recorded); otherwise `E_CONN_ARCHIVE`. Top-level files are an `E_CONN_ARCHIVE` error. The entry's folder must hold `marker`; rename it into `<primary>/<class>/<folder>`; an existing folder is renamed aside and deleted only after the new one is in place. **Several top-level folders (amended 2026-10-24):** an archive may ship variants beside the entry's folder (the Horn 2013 zip holds `… Full` and six `… Thinned Out <n>` folders, inspected 2026-10-24); the reference implementation unzips all of them into the class folder. Every other top-level folder that holds `marker` is installed the same way into `<primary>/<class>/<its name>`, with its own install record (`name` = its folder, `variant_of` = the entry's `id`); a folder without `marker` is left out (the install events carry no notes). The archive is deleted after a successful install.
- GSP: each tar is verified, then stream-extracted (allowed members only, §3.3) into the staging folder's `vol/`, then deleted; the three small files go to the folder top; after the last tar the folder is renamed into place. A cancelled multi-file install keeps completed tars' extracted members in staging and resumes with the next file.
- Install record `<folder>/.dbsloc-install.json`: `{ id, name, version, files: [{file_name, bytes, source_digest, sha256}], source_urls, installed_at (UTC ISO 8601), installed_by (product version) }`; a variant folder (above) adds `variant_of: <id>` and is listed under its own name, never as the catalogue entry itself (the entry's installed state follows its own folder only).

### 4.7 Licence acceptance record `<primary>/licenses/<id>.json`

```json
{ "id": "cohen2023-gsp1000v2", "dataset": "GSP 1000 v2 (Cohen 2023)", "version": "Dataverse 4.0",
  "access": "cohort_terms", "licence_name": "GSP Open Access Data Use Terms (2014-Apr-22)",
  "licence_url": "https://doi.org/10.7910/DVN/KKTJQC", "licence_text_sha256": "<hex of the exact text shown>",
  "attestation": null, "accepted_by": "<OS user name or server user>", "accepted_at": "2026-10-23T09:14:02Z",
  "accepted_via": "gui" , "product_version": "<version_string()>" }
```

`attestation` (for `dua`) = the exact attestation sentence shown. `accepted_via` ∈ `gui`, `cli`, `server`, `import`. A record whose `licence_text_sha256` differs from the current catalogue text is stale: the GUI asks again before the next download (existing installed data stay usable). Records are append-only history: a new acceptance renames the previous file to `<id>.<timestamp>.json`.

### 4.8 Server

- Root from `config.toml` `connectomes_dir` (default `<data_root>/connectomes`); an optional read-only `leaddbs_connectomes_dir`. The sanitizer adds the token `<connectomes>` (`docs/SERVER_CLIENT_PLAN.md` §7).
- Downloads run on the server, never through the client: `GET /api/v1/connectomes` (catalogue + state), `POST /api/v1/connectomes/{id}/fetch` with body `{accepted_by, licence_text_sha256, attest}` (the server checks the hash equals the current text), progress over SSE, `POST …/cancel`. The same lock, record (`accepted_via: "server"`) and polite rules apply; at most one connectome transfer per server. Because server v1 has no authentication (WP6 deferred), the fetch endpoint is disabled unless `allow_connectome_download = true` in `config.toml`.
- Connectome files are never in the file-serving allowlist; only derived maps under a subject's derivatives are downloadable.

### 4.9 Offline behaviour

Everything except `fetch` and `info --remote` works offline. A fetch without network fails after the retry policy with `E_CONN_HOST` ("cannot reach <host>: <reason>"), keeping any partial. Analyses use installed or registered folders only and never try to download implicitly.

---

## 5. Structural seed maps (per subject)

### 5.1 Seeds from a stimulation (`07` outputs)

The seed is built per subject and stimulation label from the per-side VTA files of `07` in template space (`stimulations/<space>/<label>/sub-<id>_sim-<kind>_model-<model>_hemi-{R,L}.nii`; `07` writes `.nii`, readers also accept `.nii.gz` in imported Lead-DBS folders; aligned 2026-10-23 with `07` §5.1), `kind` = the setting *Seed from* ∈ `binary` (default, the reference's `prefs.lcm.vatseed`), `efield`, `efieldgauss` (the last two exist only for `07` model `efield`):

1. Resample each side's image onto the **seed grid** `bb`: 0.22 mm isotropic, 492 × 492 × 265 voxels, 0-based origin (−53, −63, −25) mm, axis-aligned RAS (the reference's `templates/space/MNI152NLin2009bAsym/bb.nii`, header read 2026-10-23). Interpolation: nearest neighbour for `binary`, trilinear for `efield` / `efieldgauss`; outside the source volume = 0; NaN → 0.
2. Sum the sides voxel-wise → `S`. A missing side (no stimulation on it) contributes nothing; both missing → `E_SEED_EMPTY`.
3. Write `S` as `…_seed-dMRI.nii` (float32), cropped to the bounding box of non-zero voxels plus a rim of `round(2 mm / mean voxel size)` voxels (affine translated; values unchanged).
4. Write side splits: `…_seed-dMRI_hemi-R.nii` keeps voxels whose centre has x ≥ 0 (voxels with x < 0 zeroed), `…_hemi-L.nii` keeps x ≤ 0 (a voxel centred exactly on x = 0 stays in both, as in the reference).

The seed of §5.2 may also be any user NIfTI (CLI `--seed <file>`), in the connectome's space.

### 5.2 Fibre selection

Let the seed image have affine `A_s` (sform if `sform_code > 0`, else qform; 0-based voxel indices), voxel sizes h = (h_x, h_y, h_z) (absolute column norms), and values `S(v)`.

- Seed voxels: `V = { v : S(v) finite and S(v) ≠ 0 }`, weights `w_v = S(v)`, centres `c_v = A_s · v`.
- Search radius `r = (h_x + h_y + h_z) / 6` (half the mean voxel size).
- Fibre f **touches** seed voxel v iff some point p of f satisfies `‖p − c_v‖_∞ ≤ r` (Chebyshev / max-norm distance in mm, boundary inclusive). For the 0.22 mm `bb` grid this means "a fibre point lies inside the voxel cube (closed)". A fibre whose consecutive points straddle a small seed without a point inside it is not selected (reference behaviour; points of downloaded connectomes are typically ≥ 0.5 mm apart, **unverified** spacing).
- `V_f` = set of seed voxels touched by f (each voxel once, however many points of f lie near it).
- **Fibre strength** `s_f = (1/|V_f|) Σ_{v ∈ V_f} w_v` when `V_f ≠ ∅`, else 0. A fibre is **connected** iff `s_f ≠ 0` (so weights that average to exactly 0 disconnect the fibre, as in the reference). For a binary seed, `s_f = 1` for every touching fibre.
- If no fibre is connected: warning *No connected fibres for seed <file>*, no map is written, the job reports `skipped` for that seed.

Implementation (not normative): candidate fibres = spatial-index cells (§3.5) overlapping the seed bounding box expanded by r; for each candidate point, the voxels to test are those whose index lies within `r / h_axis` of `A_s⁻¹ p` on each axis (for an oblique `A_s`, the voxel-index bounding box of the Chebyshev cube); exact test in world mm. Equivalent to the reference's k-d tree range search with the Chebyshev metric.

### 5.3 Map accumulation

Output grid G (affine `A_G`, dims n): for each connected fibre f, `U_f` = the set of distinct grid voxels `u = floor(A_G⁻¹ p + 0.5)` (round half up, 0-based) over the points p of f, dropping any u outside `0 ≤ u < n` on some axis. Then

`M(u) = Σ_{f connected, u ∈ U_f} s_f`, and 0 elsewhere.

With a binary seed, M(u) is the number of connected fibres visiting voxel u (fibre density through the seed, over the whole fibre length, each fibre counted once per voxel). Output float32 (NIfTI datatype 16), affine of G, `…_conn-<tag>_strucmap.nii` (§9).

### 5.4 Output grids

| Setting | Grid (0-based origin, mm) | Dims | Note |
|---------|---------------------------|------|------|
| `2mm` (default; the network-mapping default) | 2 mm, (−90, −126, −72) | 91 × 109 × 91 | reference `222.nii` |
| `1mm` | 1 mm, (−90, −126, −72) | 182 × 218 × 182 | `111.nii` |
| `0.5mm` | 0.5 mm, (−98, −134, −72) | 394 × 466 × 378 | `555.nii` |

(Headers of the reference's `templates/spacedefinitions/{222,111,555}.nii.gz` read 2026-10-23.) These grids are defined for MNI152NLin2009bAsym; for another space, the grid is the space's template bounding box at the chosen spacing (§11.5).

### 5.5 E-field-weighted variants

With `kind = efield` / `efieldgauss` the weights are the (resampled) E-field values, so `s_f` is the mean E over the seed voxels the fibre touches. Thresholded per-fibre E statistics (sum, peak, top 5 %) are §7, not the map. No other weighting is defined by the reference for the structural seed map.

---

## 6. Functional seed maps (per subject)

### 6.1 Seed preparation

Per side (as §5.1 step 1, but onto the connectome's own grid `vol.space`, §3.3): trilinear resampling of the side's `07` image onto the 2 mm grid (the reference resamples with FSL `flirt -applyxfm` trilinear by default, `prefs.lcm.vat2fmrimethod = 'fsl'`); NaN → 0; negative values → 0; for `efield` seeds, values below a one-level Otsu threshold of the resampled image (256 equal-width bins over [min, max] of its finite values; the threshold is the upper edge of the last bin of the lower class; the reference uses MATLAB `multithresh`, exact bin convention **unverified**) → 0. Sides summed → `…_seed-fMRI.nii` on `vol.space`, plus `_hemi-R` / `_hemi-L` splits as §5.1 step 4. If the resampled seed is all zero while the source was not (a VTA much smaller than 2 mm falling between sample points), fall back to box averaging (each 2 mm voxel = mean of the source values inside it) with a warning `seed resampled by averaging` (deviation from the reference, which writes an empty seed and warns).

### 6.2 Per-subject correlation

Inputs: seed image `S` on `vol.space` (if its affine differs from `vol.space.mat`, it is resampled trilinearly first), the voxel list `outidx` (N voxels), the selected subset (default *Full Set*; subject indices `J`), optional output mask (default: all N voxels; else the voxels of a mask image that are in `outidx`, in `outidx` order).

- Weights `w ∈ R^N`: `w_i = S(outidx_i)`; NaN, ±Inf → 0; `|w_i| < 1e−4` → 0. Seed voxels `K = { i : w_i ≠ 0 }`; `K = ∅` → `E_SEED_EMPTY`.
- For subject j ∈ J and each of its runs ρ = 1…R_j, with `X ∈ R^{T×N}` the run's time courses (voxel i in column i after the orientation rule of §3.3):
  - seed time course `x(t) = (1/|K|) Σ_{i∈K} w_i X(t, i)`;
  - for every output voxel m: `r_{jρ}(m) = Pearson(x, X(·, m))` = `Σ_t (x_t − x̄)(X_tm − X̄_m) / sqrt(Σ_t (x_t − x̄)² · Σ_t (X_tm − X̄_m)²)`; a zero denominator gives NaN.
- Subject map `r_j(m) = (1/R_j) Σ_ρ r_{jρ}(m)` (NaN if any run is NaN at m).

### 6.3 Group-of-connectome-subjects statistics (written per seed)

Over the n subjects of the subset, per voxel m, ignoring NaN entries (n_m = number of non-NaN values):

| Output | Definition |
|--------|------------|
| `desc-AvgR` | mean of `r_j(m)` |
| `desc-VarR` | population variance `(1/n_m) Σ (r_j − mean)²` |
| `desc-AvgRFz` | mean of `z_j(m) = atanh(r_j(m))` — **the map used downstream** (network mapping, `14`) |
| `desc-T` | one-sample t of `z_j(m)` against 0: `mean / (sd / sqrt(n_m))`, sd with n_m − 1 in the denominator; NaN when n_m < 2 or sd = 0 |

Before `atanh`, r is clamped to [−1 + 1e−7, 1 − 1e−7] (deviation: the reference lets |r| = 1 produce ±Inf, e.g. at a single-voxel seed's own voxel). Values are written as float32 into the `vol.space` grid at `outidx` positions (output-mask voxels only), 0 elsewhere. Optional (`--per-subject-maps`): each `r_j` as `…_conn-<label>_id-<subject id>_corr.nii`.

### 6.4 Performance and resources (informative, targets **unverified**)

- I/O-bound: GSP 1000 v2 is ≈ 1 750 run files × 125 MB compressed (219 GB) — one full pass per batch of seeds. **All seeds of a job (all subjects of a group, both hemispheres and flipped variants) are processed in one pass**: per run, inflate `gmtc` once, compute column means and inverse norms once, form the T × S seed matrix, then one (T × S)ᵀ(T × N) product. The reference processes seeds in chunks of `prefs.lcm.chunk = 10`; we batch all seeds up to a memory cap (setting, default 25 % of physical RAM for the S × N accumulators: 3 maps × S × N × 4 bytes for running sums of r, r², z, z², plus counts).
- Running statistics use Welford-style accumulation in f64 per voxel so the per-subject maps need not be stored.
- Parallelism: runs decoded on a bounded worker pool (rayon), at most 2 compressed files in flight per core to cap memory (one GSP run inflates to 137 MB). The product may use the GPU (`fastwarp-gpu` GEMM kernel) when present; CPU fallback is mandatory and must give the same values within 1e−5.
- Cancellation is polled between runs; progress = runs done / runs total.
- MAT v5 compressed variables cannot be memory-mapped; v7.3 run files (if any) are read by chunk.

---
- **Run prefetch (2026-10-25):** run files are read and inflated ahead on min(available parallelism, 6) workers, at most that many runs and 1.5 GiB of run files in flight (the first run always), consumed strictly in run order, so every output is bit-identical to a sequential pass; the first failing run in order is reported; cancel stops the workers after their current run. Measured on 8 synthetic runs: 3.3× faster than sequential.

## 7. Per-fibre values for fibre filtering, and fibre subsets

### 7.1 Inputs

Per subject and side s ∈ {R, L}: the side's E-field in template space from `07` (`…_sim-efield_model-<model>_hemi-<R|L>.nii`, values in V/m) or, for a binary analysis, the binary VTA. Threshold θ (V/m) is a group setting owned by `14` (our default 200 V/m, `14` §8.2; the reference's fibre-filtering default is **unverified**; its VTA E threshold `horn_ethresh` is in V/mm and multiplied by 1000). For a binary VTA input θ = 0.5. The reference's group layer also passes left–right flipped E-fields as extra "patients" (`…_hemidesc-FlippedFromLeft` / `FlippedFromRight`); flipping is `14`'s job, the per-fibre computation is the same.

### 7.2 Definition (per subject, side, connectome with K fibres)

Let E be the image (affine `A_E`, 0-based), `T = { v : |E(v)| > θ }` (strictly greater, absolute value). If T is empty, every value below is 0 and the fibre list is empty.

1. **Bounding-box trim:** `b_lo`, `b_hi` = component-wise min and max of the voxel-centre coordinates `A_E · v` over v ∈ T. Keep only the fibre points p with `b_lo ≤ p ≤ b_hi` on all three axes (inclusive). (Points between a boundary voxel's centre and its outer face are dropped — reference behaviour.)
2. **Voxel mapping:** each kept point → `u = floor(A_E⁻¹ p + 0.5)`; a fibre with any kept point outside the image grid is discarded entirely.
3. `U_f` = distinct voxels of fibre f's kept points; `H_f = U_f ∩ T`.
4. For every fibre with `H_f ≠ ∅`, with the multiset of values `e = { E(v) : v ∈ H_f }` (signed values; each voxel once):

| Value | Definition |
|-------|------------|
| `bin` | 1 |
| `sum` | Σ e |
| `mean` | Σ e / |H_f| |
| `peak` | max e |
| `peak5` | mean of the ⌈0.05 · |H_f|⌉ largest values of e |

Fibres with `H_f = ∅` have all values 0 and are not stored.

### 7.3 Feature matrix contract (consumed by `14`)

For connectome C (K fibres, global 0-based fibre ids in file order; for multi-tract connectomes the merged numbering of §3.4), side s and metric m ∈ {bin, sum, mean, peak, peak5}: `X_m^s ∈ R^{K × N}` over the N analysis inputs (subjects, plus flipped inputs added by `14`), zero where not connected. The connected set `C^s = { f : ∃ input with bin = 1 }`. `14` stores and uses only the rows in `C^s` (the reference keeps them as sparse matrices plus the index list and K). The per-subject file of §9 holds one column.

### 7.4 Fibre subset files (viewer and export)

A subset of fibres (e.g. the connected fibres of a seed, or the fibres a group analysis selects with their t-values) is written in the §3.2 format v1.1 as MAT v7 (Level 5, `write_mat5`): `fibers` (P' × 4 single, fibre numbers renumbered 1…K'), `idx` (K' × 1 double), `ea_fibformat = '1.1'`, `fourindex = 1`, `voxmm = 'mm'`, `vals` (K' × 1 single: the value to colour by), plus our variable `source_fiber_index` (K' × 1 double: 1-based fibre number in the source connectome) and `source_connectome` (char: tag). Lead-DBS can open the file; the 3D viewer reads it with the same reader. A subset whose `fibers` would exceed 1.5 GB is refused (`E_CONN_SUBSET_TOO_LARGE`; MAT v7 variables are limited to 2 GB). Display decimation: the viewer draws at most *Maximum fibres shown* (default 200, the reference's `prefs.d3.maxfibers`; setting up to 100 000), choosing fibres by descending |vals|, ties by ascending source index, so the choice is deterministic; it always reports *n of N fibres shown*. Rendering of fibre layers: `specs/08` §11.1.8 (amended 2026-10-25).

---

## 8. Network mapping: per-subject contract

- **Per-subject map:** structural = the `strucmap` of §5 (2 mm grid unless set); functional = `desc-AvgRFz` of §6 on the connectome's `vol.space`. The seed is the combined both-hemisphere seed of §5.1 / §6.1 (`…_seed-dMRI.nii`, `…_seed-fMRI.nii`); `14` may add left–right flipped seeds (the reference names them `…_seed-<dMRI|fMRI>_hemidesc-flipped.nii`, produced with its nonlinear flip) and requests their maps through the same contract.
- **Comparability:** all maps for one network-mapping analysis use the same connectome (and subset), seed kind and output grid; the provenance sidecar (§9.3) carries these, and `14` refuses to mix maps whose sidecars differ in any of them.
- **Re-use:** a map is up to date when its sidecar's `seed_sha256` equals the current seed file's SHA-256 and `connectome.digest` equals the current connectome digest (install-record SHA-256s, or for registered / external folders the §3.5 probe hash of `data.mat` / `dataset_volsurf.mat`) and the parameters match; otherwise it is stale and recomputed on request (shown as *stale* in the GUI, never silently reused).
- The R-map (voxel-wise correlation of maps with outcomes), its statistics and cross-validation are `14`.

---

## 9. Job contract, output names, provenance

### 9.1 Not a pipeline stage

Connectivity maps are not one of the ten pipeline stages and take no stage lock. They are subject-level derivatives computed by a **connectivity job** after the stimulation outputs of `07` exist. Front ends: the group / stimulation panels of the GUI (worker thread, progress, cancel; amended 2026-10-25: the stimulation editor has a *Connectivity* section for the open label — connectome choice, *Compute connectivity* writes the §9.2 structural outputs including the connected-fibre subset, or the functional maps for an fMRI connectome, and *Show fibres* adds the subset to the 3D view as a fibre layer coloured by strength), the CLI, and the server queue (one connectivity job at a time per server, after pipeline runs; same SSE progress). Cancellation leaves no partial output (writes go to `.partial-*` names, then rename).

CLI:

```
dbsloc connectome map <bids root> <sid>… --stim <label> --space <space> --connectome <id|name[ > subset]>
        [--seed-from binary|efield|efieldgauss] [--grid 2mm|1mm|0.5mm] [--output-mask FILE]
        [--per-subject-maps] [--seed FILE]
dbsloc connectome fibervals <bids root> <sid>… --stim <label> --connectome <id|name> --threshold <V/m>
        [--side R|L|both]
```

Exit codes as `dbsloc run` (0, 1 on any failed subject, 2 usage, 130 Ctrl-C); a JSON summary with per-subject status (`ok`, `skipped: <reason>`, `error: <code> <message>`) is printed with `--json`.

### 9.2 Output names (Lead-DBS-compatible)

Folder: `<derivatives root>/sub-<id>/stimulations/<space>/<label>/` (`specs/01` output root rule; `<space>` = template space id, e.g. `MNI152NLin2009bAsym`; Lead Group labels are `gs_<guid>`). `<model>` = the Lead-DBS file slug of the `07` model (`07` §1.1: `simbio` for `efield`, else `kuncel`, `maedler`, `dembek`), the same value as in the VTA file names (aligned 2026-10-23 with `07` §1.1, §5.1). Names follow the reference's BIDS-entity rule: a new entity is inserted before the suffix, so `conn-` follows `seed-`.

| Output | Name |
|--------|------|
| Structural seed | `sub-<id>_sim-<kind>_model-<model>_seed-dMRI.nii`, `…_seed-dMRI_hemi-L.nii`, `…_hemi-R.nii` |
| Functional seed | `sub-<id>_sim-<kind>_model-<model>_seed-fMRI.nii` (+ `_hemi-L` / `_hemi-R`) |
| Structural map | `sub-<id>_sim-<kind>_model-<model>_seed-dMRI_conn-<tag>_strucmap.nii` |
| Functional maps | `sub-<id>_sim-<kind>_model-<model>_seed-fMRI_conn-<label>_desc-{AvgR,VarR,AvgRFz,T}_funcmap.nii` |
| Per connectome-subject r | `…_seed-fMRI_conn-<label>_id-<subject id>_corr.nii` (optional) |
| Per-fibre values (ours) | `sub-<id>_sim-efield_model-<model>_hemi-<R\|L>_conn-<tag>_fibervals.mat` |
| Connected-fibre subset (ours) | `sub-<id>_sim-<kind>_model-<model>_seed-dMRI_conn-<tag>_fibers.mat` |

- Structural `<tag>` = `dataset_info.json` `tag`; when the folder has no `dataset_info.json` (the reference then fails), the folder name with every character outside `[A-Za-z0-9]` removed, with a warning.
- Functional `<label>` = `<tag>` + `x` + the subset name with every character outside `[A-Za-z0-9]` removed; *Full Set* when no subset is chosen (`GSP1000v2xFullSet`, `PPMI74P15CxPatients`).
- Maps are float32 uncompressed `.nii` (as the reference); readers accept `.nii.gz` too. A seed given as a non-BIDS file name gets the suffix appended to its stem (`<stem>_conn-<tag>_strucmap.nii`) in the seed's folder.
- `…_fibervals.mat` (MAT v7): `fiber_index` (n × 1 double, 1-based global fibre numbers, ascending), `bin` (uint8), `sum`, `mean`, `peak`, `peak5` (single), `total_fibers` (K), `threshold_vpm`, `connectome_tag`, `connectome_digest`, `efield_file`, `efield_sha256`.

### 9.3 Provenance sidecar

Every map, seed, `fibervals` and subset file gets `<file without extension>.json`: `{ software: version_string(), created_at, connectome: { id, name, tag, subset, class, version, digest, root_kind: primary|registered|external, licence_record: <path or null>, citation, acknowledgement }, seed: { file, sha256, kind, radius_mm, grid }, params: {…}, inputs: [{file, sha256}] }`.

---

## 10. Acceptance criteria and tests

All tests run offline on synthetic data created by the tests; no connectome is downloaded in CI.

**Readers**

1. Tiny structural fixtures generated by a committed script (`scripts/make_connectome_fixtures.py`, run once with `uv run --with h5py --with numpy`; h5py is not a project dependency) and committed (< 100 KB each): (a) MAT v7 compressed, `fibers` 7 × 4 single, `idx` [3; 4], `voxmm 'mm'`; (b) the same data as MAT v7.3 (HDF5 with the 512-byte MATLAB header; `fibers` stored as dataspace (4, 7), chunked + deflate); (c) a `'vox'` file with `mat`; (d) a file whose fibre-number column disagrees with `idx` → `E_CONN_FORMAT`. (a)–(c) read to identical points (within f32) and fibre ranges.
2. `.trk`: a synthetic file with a known `vox_to_ras` (non-identity, with a flip), 2 tracks, 1 scalar per point and 1 property per track; world points equal the voxel-centre formula within 1e−5 mm; `n_count = 0` reads to EOF; a byte-swapped header is detected.
3. Functional fixture: `dataset_volsurf.mat` (v7.3) with a 6 × 5 × 4 grid, non-sorted `outidx` of 20 voxels, 3 subjects with 1–2 runs; run files in both orientations (N × T and T × N, MAT v5 compressed); `dataset_info.json` in Windows-1252 with a non-ASCII citation. Linear-index conversion and orientation detection are exercised.

**Structural seed map**

4. Seed: a 3 × 3 × 3 binary cube of 1 mm voxels centred at the origin (r = 0.5 mm). Fibres along x: f1 through the cube centre (y = z = 0); f2 at y = 1.5 mm (Chebyshev distance exactly 0.5 mm from the outer voxel centres → touches, boundary inclusive); f3 at y = 1.6 mm (does not touch). On the 2 mm grid, M = 1 on the voxels visited by f1 and f2 (2 where both visit the same voxel), 0 elsewhere; f3 never contributes.
5. Weighted seed: f1 touches voxels with weights 1 and 3 → s = 2; a fibre touching one voxel of weight −1 and one of +1 → s = 0 → not connected. A fibre with several points in the same output voxel adds its strength once.
6. Empty result writes no map and reports `skipped`.

**Per-fibre values**

7. E-field on a 1 mm grid with a fibre crossing voxels valued 300, 500, 100 V/m and θ = 200: `bin` 1, `sum` 800, `mean` 400, `peak` 500, `peak5` 500 (⌈0.05·2⌉ = 1). A fibre ending just outside the suprathreshold bounding box only through points beyond the extreme voxel centre is not counted (rule 7.2.1). Negative E values: threshold on |E|, statistics on signed values.

**Functional seed map**

8. Synthetic run data with T = 50, a seed time course s, and voxel time courses `a_m s + b_m n_m` with s and n_m orthonormal and zero-mean, so `r = a_m / sqrt(a_m² + b_m²)` exactly; AvgR, VarR, AvgRFz (atanh of the per-subject means) and T match closed-form values within 1e−5; a constant voxel gives NaN r and is excluded from n_m; a subset restricts the subjects; the GPU path (when available) equals the CPU path within 1e−5.
9. Seed preparation: a 0.5 mm-radius binary VTA that trilinear sampling misses on the 2 mm grid triggers the averaging fallback with the warning; negative seed values are zeroed.

**Download and catalogue (local test HTTP server or file-backed hook)**

10. `download.php` style 302 → file; `Range` honoured (206) and resumed after a simulated disconnect; Range ignored (200) → restart from 0; `OC-Checksum` mismatch → stop before writing; size or digest mismatch → `E_CONN_CHECKSUM`, partial removed; 429 with `Retry-After` → one wait (test clock) then resume; 403 → no retry; a second concurrent fetch → `E_CONN_BUSY`; cancel keeps the partial; terms not accepted → refusal without any request; `dua` without attestation → refusal; free space too small → `E_CONN_DISK` before any request; a zip with `../x` → `E_CONN_ARCHIVE`; the User-Agent header equals the product string.
11. Catalogue: ids unique; every entry has bytes > 0, a `source_digest`, licence text, citation, instructions; `access = dua` entries have a dataset URL; GSP has 38 files summing to 218 932 842 997 bytes.
12. GUI: the card renders each access class (screenshot mode with synthetic catalogue, `DBSLOC_APP_SCREENSHOT_ACTION=connectomes`); *Download* stays disabled until the required ticks are set.

**Performance (measured, not gating)**: structural map for one seed on HCP 32 from the cache; one GSP 1000 v2 pass for 100 seeds on NVMe. Results recorded in `docs/` when first run.

---

## 11. Edge cases and limits

1. **Huge files.** HCP 32 has 396.6 M points (6.3 GB as f32 x/y/z/id). Readers stream by chunk; nothing loads all points at once unless the cache is memory-mapped. Out of memory is reported as `E_CONN_MEMORY` with the suggestion to enable the cache.
2. **Missing dataset.** A connectome named in a request but absent from every root → `E_CONN_MISSING` "connectome <name> not found in <roots>"; the GUI offers the catalogue entry. A registered folder that vanished is shown as *Missing* and skipped.
3. **Partial download.** Kept in `.partial-<id>/` for resume (30-day limit, §4.4); never discovered as a connectome; `list` shows *Partial download (NN %)*.
4. **Licence not accepted.** `fetch` refuses (`E_CONN_TERMS`); analyses on an installed or registered folder without a record still run (the user may have obtained the data elsewhere) and their sidecars say `licence_record: null`; the GUI shows *No licence record* on such folders.
5. **Different template space.** Every connectome has a space (catalogue, else `MNI152NLin2009bAsym` for Lead-DBS folders, overridable by a `space` key in our install record or registry entry). A stimulation in another space S is mapped into the connectome's space through the template bridge (`specs/04` / `specs/08` §5: the same bridge used to warp atlases) — seeds resampled with nearest neighbour (binary) or trilinear (E-field) — and the outputs are written under `stimulations/<connectome space>/<label>/` with a note in the sidecar. When the connectome space is `MNI152NLin2009bAsym` and the `vat` stage already wrote the reference-space copy there (`07` §4 step 3), the seeds are read from that copy and not resampled again (aligned 2026-10-23). No bridge between the two spaces (e.g. the pediatric set) → `E_CONN_SPACE`.
6. **Seed grid mismatch.** Functional seeds not on `vol.space` are resampled (§6.2); structural seeds are used on their own grid.
7. **Fibres outside the output grid** are dropped point-wise (§5.3); fibres outside the E-field grid after the box trim are dropped fibre-wise (§7.2).
8. **NaN / Inf** in seeds → 0; in E-fields → treated as below threshold; in time courses → the affected Pearson r is NaN (excluded via n_m).
9. **Subjects with one run** or with differing T across runs: handled per run; a run with T < 3 is skipped with a warning.
10. **External Lead-DBS root on a slow network share:** reads are streamed; the cache (in the primary root) is recommended; nothing is written to the external root.
11. **Catalogue drift.** A server answering a different size or SHA-1 than the pin → `E_CONN_CHANGED`; the fix is a catalogue update, never a silent accept.
12. **Disk full during extraction** → staging removed, archive kept for a retry, `E_CONN_DISK`.

Error codes: `E_CONN_MISSING`, `E_CONN_FORMAT`, `E_CONN_SPACE`, `E_CONN_TERMS`, `E_CONN_BUSY`, `E_CONN_HOST`, `E_CONN_SIZE`, `E_CONN_CHANGED`, `E_CONN_CHECKSUM`, `E_CONN_DISK`, `E_CONN_ARCHIVE`, `E_CONN_MEMORY`, `E_CONN_SUBSET_TOO_LARGE`, `E_SEED_EMPTY`.

### 11.1 Decisions taken in this spec (2026-10-23, Phase-B author; owner decisions of the same date are locked in §2–§4)

| # | Decision |
|---|----------|
| D1 | Read Lead-DBS formats natively; the cache (§3.5) is optional, never required, never written into a connectome folder. |
| D2 | Three access classes; `dua` datasets (MGH-USC HCP 32, both PPMI sets) are downloadable only after an explicit attestation, else imported — **to be confirmed by legal review**. |
| D3 | Legacy fibre formats, `.fib.gz`, `fMRI_matrix` and surface time courses are out of scope for v1. |
| D4 | `.trk` uses the TrackVis voxel-centre convention (−0.5 voxel), not the reference converter's. |
| D5 | Fisher-z input clamped to |r| ≤ 1 − 1e−7; empty trilinear functional seed falls back to box averaging. |
| D6 | Multi-tract merge order = file name order. |
| D7 | Maps written as uncompressed `.nii` with Lead-DBS names; our own per-fibre and subset files are MAT v7 so Lead-DBS users can open them. |
| D8 | Partial downloads are kept for resume (cancel does not delete), unlike the template / backdrop fetch which removes staging. |
| D9 | Our SHA-256 pins are added after the first verified download, verified against the host's SHA-1 / MD5. |
| D10 | **Owner decision 2026-11-10 (open-source release), supersedes D2 and the owner decision of 2026-10-23 in the implementation notes:** `dua` entries are import-only with step-by-step instructions (§2.3, §2.4, §4.2, §4.3); dTOR-985 is fetched from the authors' figshare records; NKI-RS 169 and Pediatric 100 stay downloadable with a non-commercial notice (§2.4); SHA-256 pins are set for every file a maintainer has hashed (§4.5). |

---

### Implementation notes (Phase C, 2026-10-23; normative where this spec left a choice open)

- **§2.2 / §2.3.** `connectome_catalog.rs` holds 11 entries with the SHA-1 pins of the sciebo share and the 38 GSP MD5 pins. Folder names of dTOR-985, Pediatric 100, Horn 2013, NKI-RS 169 and PPMI 85 are unverified (`folder_verified = false`: the archive's single top-level folder is accepted). Licence texts of the `dua` / `cohort_terms` entries are quoted excerpts plus the URL until a maintainer pastes the verbatim terms; the acceptance record hashes whatever the catalogue holds.
- **§3.2 / §3.3.** Fibre sets and run files are read through streaming f32 column reads (`fastwarp_matfile::stream_columns_f32`, `MatFile::stream_numeric_f32`; contiguous, compact and chunked + deflate / shuffle / Fletcher32 HDF5 layouts), so no f64 copy exists; memory per read is bounded by the chunk. `GSP1000.txt` is ignored; a `vals` of the wrong length is `E_CONN_FORMAT`; `.trk` ignores `voxel_order` and uses the TrackVis voxel-centre convention (D4). Multi-tract merge gives 1.0 to files without `vals`. v7.3 reads of real connectome files are untested (no fixture).
- **§3.5.** (Superseded by the 2026-10-25 amendment of §3.5: dense grid, Morton order, cache dir.) The in-memory index is a sorted list of occupied cells anchored at the world origin; `fibres_through_mask` takes the fibre set too and checks each candidate point against its nearest mask voxel. The on-disk cache is a type and path rule only (`cache build|clear` reports "not available yet"); seed maps select fibres through `FibreIndex::fibres_through` plus an exact Chebyshev boundary test (1e-6 mm tolerance) so a fibre on the §5.2 boundary counts.
- **§4.** `fetch` resolves the redirect through lead-dbs.org on every new fetch, keeps the partial on cancel / network failure / `E_CONN_CHANGED`, deletes it on a checksum or digest mismatch of a complete file (§4.5), writes the licence record after the lock and disk checks and before the first request, and accepts `--max-rate`. `import` recognises a folder by its marker files only. The CLI also offers `--root` on every subcommand and `--json` on `list` / `info` / `fetch` / `import`. The GUI is a *Connectomes…* window (top bar): links + instructions for every entry, acceptance tick, attestation tick for `dua`, download / resume card, *Import folder…* / *Import archive…*, *Forget*; *Remove* of an installed folder, the max-rate setting and a re-prompt on a stale record are not implemented.
- **§5–§7.** Fibre subset and `fibervals.mat` files store numbers as doubles (`Mat5Value` has no single class); a subset over the size limit is written with a warning rather than refused. NaN statistics are written as 0 in NIfTI. An invalid seed fails only that subject in a functional batch; a cancel discards the batch. Extra CLI options: `--model`, `--no-fibre-file`, `--seed-from`, `--threshold` (default 200 V/m).
- **§8 / §9 / §11.** The connectome digest is the probe hash (size + first and last 16 MiB) of the main file; a multi-tract folder hashes its per-file digests in name order. Re-use / stale detection of existing maps is not implemented (sidecars carry `seed.sha256`, `connectome.digest`, `params`). A stimulation in another template space returns `E_CONN_SPACE` (the bridge of `specs/04` / `08` is not applied). The non-linear flipped seed of §8 is not defined here; the job accepts any seed through `--seed FILE`. §6.4 GPU path: CPU only.

- **§4 licence texts (2026-10-23).** The catalogue carries the owner's terms verbatim with source URL and fetch date (`resources/licenses/connectomes/*.txt`, compiled in with `include_str!`); each file starts with `catalogue_ids:`, `source_url:`, `fetched:`, `title:`, `attribution:`. The acceptance record stores `licence_text_sha256` (LF-normalised) and `terms_fetched`; a differing hash means not accepted. No licence text exists for lead-dbs.org hosting, Gibbs 19 (only the Data in Brief statement), NKI-RS (INDI statements; NITRC lists GPL-2) or Pediatric 100 (INDI / CoRR policy). The PPMI DUA may change "effective immediately upon posting", so it is re-checked at every release. **Owner decision (2026-10-23):** the PPMI entries stay `dua` with attested download from lead-dbs.org for a user who holds an approved PPMI DUA (the research reading of Appendix A recommended import-only; the owner accepted the attestation route); the Pediatric 100 source cohort (100 children vs the 187 adults on the CoRR NYU 2 page) needs the authors' confirmation.

## 12. References

- Horn A, Ostwald D, Reisert M, Blankenburg F. The structural–functional connectome and the default mode network of the human brain. *NeuroImage* 102 (2014) 142–151 (Gibbs-tracking group connectome; cited by Lead-DBS as Horn 2013, PubMed 24099851).
- Horn A, Blankenburg F. Toward a standardized structural–functional group connectome in MNI space. *NeuroImage* 124 (2016) 310–322 (NKI-RS 169).
- Horn A, Kühn AA, Merkl A, Shih L, Alterman R, Fox M. Probabilistic conversion of neurosurgical DBS electrode coordinates into MNI space. *NeuroImage* 150 (2017) 395–404 (MGH-USC HCP 32 processing).
- Setsompop K, et al. Pushing the limits of in vivo diffusion MRI for the Human Connectome Project. *NeuroImage* 80 (2013) 220–233.
- Ewert S, et al. Toward defining deep brain stimulation targets in MNI space: a subcortical atlas based on multimodal MRI, histology and structural connectivity. *NeuroImage* 170 (2018) 271–282 (PPMI 85; Lead-DBS cites Ewert 2017, PubMed 28536045).
- Marek K, et al. The Parkinson Progression Marker Initiative (PPMI). *Prog Neurobiol* 95 (2011) 629–635.
- Horn A, Reich M, Vorwerk J, et al. Connectivity predicts deep brain stimulation outcome in Parkinson disease. *Ann Neurol* 82 (2017) 67–78 (PPMI 74/15 functional; seed-map method).
- Loh A, Boutet A, et al. A functional connectome of Parkinson's disease patients prior to deep brain stimulation: a tool for disease-specific connectivity analyses. *Front Neurosci* (2022) (**unverified** citation details); data: Zenodo doi:10.5281/zenodo.4310183.
- Elias GJB, et al. A large normative connectome for exploring the tractographic correlates of focal brain interventions. *Sci Data* 11 (2024), doi:10.1038/s41597-024-03197-0; data: figshare doi:10.6084/m9.figshare.c.6844890.
- Al-Fatly B, et al. *NeuroImage: Clinical* (2023), doi:10.1016/j.nicl.2023.103449 (pediatric functional connectome).
- Yeo BTT, et al. The organization of the human cerebral cortex estimated by intrinsic functional connectivity. *J Neurophysiol* 106 (2011) 1125–1165; Holmes AJ, et al. Brain Genomics Superstruct Project initial data release. *Sci Data* 2 (2015) 150031.
- Cohen A, Al-Fatly B, Horn A. GSP1000 Preprocessed Connectome for Lead DBS. Harvard Dataverse (2023), doi:10.7910/DVN/KKTJQC.
- Yeh F-C, et al. Population-averaged atlas of the macroscale human structural connectome and its network topology. *NeuroImage* 178 (2018) 57–68.
- Baldermann JC, et al. Connectivity profile predictive of effective deep brain stimulation in obsessive-compulsive disorder. *Biol Psychiatry* 85 (2019) 735–743 (fibre filtering).
- Fisher RA. On the "probable error" of a coefficient of correlation deduced from a small sample. *Metron* 1 (1921) 3–32.
- TrackVis file format: `http://trackvis.org/docs/?subsect=fileformat` (public specification).
- MATLAB MAT-file format (Level 5) and MAT v7.3 (HDF5) conventions: `specs/11` §3.1, `fastwarp/crates/fastwarp-matfile`.
- Data use terms: WU-Minn HCP Open Access Data Use Terms; MGH-USC HCP Data Use Agreement (2014); PPMI Data Use Agreement; GSP Open Access Data Use Terms (2014-Apr-22); NKI-RS / INDI usage agreement.

---

## Appendix A — GSP 1000 v2 files (Harvard Dataverse doi:10.7910/DVN/KKTJQC, version 4.0; queried 2026-10-23)

Download: `https://dataverse.harvard.edu/api/access/datafile/<file id>`.

| File | File id | Bytes | MD5 |
|------|--------:|------:|-----|
| `dataset_info.json` | 11539066 | 4 733 | `ff8ce4749fcdb74b79dcc4b75d83320e` |
| `dataset_info.mat` | 11058539 | 2 228 896 | `3e8f5aefeae5e68cd015dc11fccd83bd` |
| `dataset_volsurf.mat` | 11058538 | 2 210 520 | `452c75d43d0bd6c5ddf5683ed35024d3` |
| `GSP1000_leadDBS_00.tar` | 7611704 | 6 142 536 704 | `5877590a6dcb9b5879368f9702deac9e` |
| `GSP1000_leadDBS_01.tar` | 7611708 | 6 269 285 376 | `929f54b3403312397e261a56fc71429a` |
| `GSP1000_leadDBS_02.tar` | 7611709 | 6 269 563 392 | `a3780effda50da654dff8fc5c0a65965` |
| `GSP1000_leadDBS_03.tar` | 7612743 | 6 270 397 440 | `60da1d99090c56e1f5668dbfbe191bbd` |
| `GSP1000_leadDBS_04.tar` | 7612744 | 6 269 744 128 | `3014d3afaaa2d782cb5e778b659eb7ce` |
| `GSP1000_leadDBS_05.tar` | 7612746 | 6 269 561 344 | `006413c1e15c58014c1cd11433a0c78c` |
| `GSP1000_leadDBS_06.tar` | 7612747 | 6 268 104 704 | `f71cb449562f0346d7bb11b2df568508` |
| `GSP1000_leadDBS_07.tar` | 7612756 | 6 269 067 264 | `30c12c5e5c26de530c1a983e0d4a38b7` |
| `GSP1000_leadDBS_08.tar` | 7612817 | 6 268 318 720 | `4a01d1421303def467f0dc796d71b0ed` |
| `GSP1000_leadDBS_09.tar` | 7612844 | 6 269 735 936 | `856493a997f5f42db2f055e28b3776cc` |
| `GSP1000_leadDBS_10.tar` | 7612913 | 6 270 212 608 | `337bb859769216ffb14c6da86ebfd675` |
| `GSP1000_leadDBS_11.tar` | 7612914 | 6 269 484 544 | `0f9e0d0e93bbb1a534fb93bf5db8d1d5` |
| `GSP1000_leadDBS_12.tar` | 7612917 | 6 269 851 136 | `97ebf1b420fb32d9b13c3c42d7d011f2` |
| `GSP1000_leadDBS_13.tar` | 7612961 | 6 269 012 480 | `5b578cc761008f43d4f366b75fdac800` |
| `GSP1000_leadDBS_14.tar` | 7612968 | 6 269 215 232 | `4b60d1efb50966a3a0b403e87044dcff` |
| `GSP1000_leadDBS_15.tar` | 7612971 | 6 270 123 520 | `1399ff6bb1112a2ca1334beca02f7a92` |
| `GSP1000_leadDBS_16.tar` | 7612973 | 6 269 361 664 | `7ec037f3c541743834f5323706300849` |
| `GSP1000_leadDBS_17.tar` | 7613009 | 6 271 344 640 | `37810cd1ea212e65d1fad4380bd04ad8` |
| `GSP1000_leadDBS_18.tar` | 7613097 | 6 269 627 904 | `6036e2df1271f05679608f87bd1e50db` |
| `GSP1000_leadDBS_19.tar` | 7613167 | 6 269 739 520 | `9404bc1103f1f23ea4fe93bde2e79e82` |
| `GSP1000_leadDBS_20.tar` | 7613346 | 6 268 455 936 | `c64012363af589789d630d0474f50876` |
| `GSP1000_leadDBS_21.tar` | 7613672 | 6 269 213 184 | `277cbcf3d5972c03ac59070beee14f17` |
| `GSP1000_leadDBS_22.tar` | 7613740 | 6 269 089 792 | `211443b7202dbbe0325845d74bb78dbc` |
| `GSP1000_leadDBS_23.tar` | 7613804 | 6 269 406 720 | `d37b187ff1a327cfa34d5d69e7a07ab1` |
| `GSP1000_leadDBS_24.tar` | 7613897 | 6 269 859 840 | `4b5bf38885faa83fddd52916b2fb692b` |
| `GSP1000_leadDBS_25.tar` | 7613968 | 6 269 807 616 | `96e341d4e7d2a93054432021d82674fb` |
| `GSP1000_leadDBS_26.tar` | 7615347 | 6 269 944 832 | `6aeb586fcbcbd0b80cb778ac6c425f37` |
| `GSP1000_leadDBS_27.tar` | 7614038 | 6 269 011 456 | `959f7e97373f4525b8497a606c51c200` |
| `GSP1000_leadDBS_28.tar` | 7614101 | 6 268 562 432 | `2146e58ef7246a322f243140966afc17` |
| `GSP1000_leadDBS_29.tar` | 7614181 | 6 268 871 168 | `21373ebb941ff9d4d3ef7fc3129575ad` |
| `GSP1000_leadDBS_30.tar` | 7614259 | 6 270 990 336 | `7289338addfb96a97baeeb7952bd2c89` |
| `GSP1000_leadDBS_31.tar` | 7614309 | 6 268 421 120 | `c46428d65253e9621cff24d13c76aa20` |
| `GSP1000_leadDBS_32.tar` | 7614378 | 6 269 055 488 | `10c65ff4a1641d9891758bd23f53be69` |
| `GSP1000_leadDBS_33.tar` | 7614459 | 6 269 453 312 | `91baead8259295060a85a48f76762a57` |
| `GSP1000_leadDBS_34.tar` | 7614525 | 5 893 967 360 | `1eec3e560c942af1aa3beb11af92437a` |

## Appendix B — other files on the Lead-DBS share (for maintainers; not in this catalogue)

The same `PROPFIND` listed backdrops, atlases and template packs (e.g. `7T_100um_Edlow_2019.mat` 2 166 080 767, `fliplr.zip` 1 242 134 759 — the reference's nonlinear left–right flip transform, relevant to `14` mirroring, `genetics.zip`, `MNIPD25.zip`, `CIT168.zip`, `leaddbs_sample_pt.tar.gz`) and an `older_versions/` folder. Their licences are outside this spec.
