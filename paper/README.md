# JOSS paper

`paper.md` and `paper.bib` are the manuscript for the
[Journal of Open Source Software](https://joss.theoj.org). `paper.md` holds the required headings
and, under each, a comment on what JOSS expects there. Replace the comments with text and fill
the `TODO(author):` fields.

## Limits

The whole paper must be 750 to 1750 words. Full-length papers and API documentation are not
allowed. Rules for the format:
<https://joss.readthedocs.io/en/latest/paper.html>.

## Build the PDF

Locally, with the Open Journals image (Docker), from the repository root:

```bash
docker run --rm --platform linux/amd64 \
  --volume "$PWD/paper:/data" --user "$(id -u):$(id -g)" \
  --env JOURNAL=joss openjournals/inara@sha256:a0414b8b72fd8923917ede614d340d98dc7aa3102aabc2e955e9c02a6100fd62
```

It writes `paper/paper.pdf`. On Windows PowerShell, replace the volume and user options with
`--volume "${PWD}/paper:/data"` and drop `--user`.

In GitHub, `.github/workflows/paper.yml` builds the draft PDF when a pull request or a push to
`main` changes `paper/**`, and on manual runs (`gh workflow run paper`). Download the `paper`
artifact from the run.

## Before you submit

From <https://joss.readthedocs.io/en/latest/submitting.html>:

- The repository has been public for more than six months before submission, with active
  development over that time; a repository made public just before submission is not accepted.
  The history must show steady work, not one burst of commits.
- The licence is OSI-approved (Apache-2.0).
- Anyone can clone the code, browse it and read the issue tracker without registration, and open
  issues and propose changes without approval or payment.
- The software is research software with an obvious research application. The paper reports no
  new research results.
- There is evidence of research use, at least by the developers, ideally by others.
- The software is feature-complete, packaged to community standards and built to be extended.
- The submitter is a major contributor and has a GitHub account.
- The paper files are in the same repository as the software.
- A single-author project shows a meaningful commit history, tagged releases or a changelog,
  tests and CI, documentation, a CONTRIBUTING file and stated support expectations.
- AI usage, conflicts of interest and funding are disclosed.
- After acceptance: tag a release, archive the repository (for example on Zenodo), and post the
  version and DOI in the review thread.

The reviewer checklist is at <https://joss.readthedocs.io/en/latest/review_checklist.html>.
