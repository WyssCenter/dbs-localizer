# Releasing FastWarp

## One-time setup

PyPI trusted publishing needs no API token. Register the project as a pending publisher before
the first release (PyPI: *Your account*, *Publishing*, *Add a new pending publisher*):

| Field | Value |
| --- | --- |
| PyPI project name | `fastwarp` |
| Owner | `WyssCenter` |
| Repository name | `fastwarp` |
| Workflow name | `wheels.yml` |
| Environment name | `pypi` |

In GitHub, create the environment `pypi` (*Settings*, *Environments*) before the first tag and
limit its deployment tags to `v*`; an environment that GitHub creates on first use has no
protection. Add a tag ruleset so that only maintainers can create `v*` tags. Add required
reviewers to the environment if a release should need a manual approval. The first successful
upload turns the pending publisher into a normal one.

### Trial upload to TestPyPI

TestPyPI is a separate site with its own account. Register a second pending publisher at
<https://test.pypi.org/manage/account/publishing/> with the same values, except the environment
name `testpypi`. In GitHub, create the environment `testpypi` limited to the branch `main`.
Then run the workflow by hand on `main` with the box ticked:
`gh workflow run wheels.yml --ref main -f testpypi=true`. It uploads the version in `Cargo.toml`;
TestPyPI, like PyPI, refuses a version it already has, so bump the version before a second
trial. Install the result with
`pip install --index-url https://test.pypi.org/simple/ --extra-index-url https://pypi.org/simple/ fastwarp==<version>`;
without the version, pip takes the highest version from either site and skips pre-releases.

A release candidate (for example `0.1.0-rc.1` in `Cargo.toml`, `0.1.0rc1` on TestPyPI) goes
only through this manual run. Never push a tag for it: a tag publishes to PyPI.

## Release steps

`scripts/release.py` does the repetitive steps. It runs on Linux, macOS and Windows. Run it with
`uv run scripts/release.py ...`: uv takes the Python interpreter from `.python-version` and the
script's own header (Python 3.11 or later, no packages) and does not build the project. Do not
run it with a bare `python`, which may be any interpreter. It needs `git` and an authenticated
`gh` on the PATH. It always works from `origin/main` and the GitHub repository of `origin`, asks
before each step that changes something, and `--dry-run` prints the changes of `prepare` and
`finish` without making them.

Before you start, by hand: check the pinned tools for newer versions (see the end of *What the
workflow does*), and search `README.md` and `docs/` for sentences that speak of the coming
release as a future one.

1. `uv run scripts/release.py prepare <version>` (`X.Y.Z`, or `major`, `minor` or `patch`) opens
   the release pull request from the branch `release/<version>`:
   - `version` in `Cargo.toml` (`[workspace.package]`; the wheel takes its version from there),
     in `Cargo.lock` and in `CITATION.cff`;
   - `CHANGELOG.md`: `## Unreleased` becomes `## <version> (<date>)` under a new, empty
     `## Unreleased`. The GitHub release notes are the section whose heading names the tagged
     version; the release fails if there is none;
   - the version in the BibTeX entry of `docs/citing.md` and in the download commands of
     `docs/cli.md`.
2. Squash-merge the pull request when its checks are green, `full.yml` included (the ruleset
   requires only `lint` and `test`). Merge nothing else until the tag is pushed.
3. `uv run scripts/release.py tag` checks the head of `origin/main`: it is the release commit
   (its parent has another version and `## Unreleased` is empty), the files above agree on the
   version, the tag does not exist, `ci.yml` is green for the push of the commit, and a manual
   run of the release workflow on `main` (`gh workflow run wheels.yml --ref main`) is green on
   the commit. That run builds the wheels, the programs and the installers, installs every
   program except macOS x86_64 with its installer and runs the installer tests on macOS, without
   publishing anything; the script offers to start it and waits for it. Then, after you type the
   version, it creates the annotated tag `v<version>` on that commit and pushes it. The tag
   publishes to PyPI.
4. When the release is published, `uv run scripts/release.py finish <version>` opens a pull
   request that pins `benchmarks/fastwarp-pypi.txt` to the new wheels. It takes their SHA-256
   from PyPI and refuses them unless they equal those in `SHA256SUMS` of the GitHub release.

A release candidate is not released with the script: set its version by hand and use the trial
upload above.

## What the workflow does

`.github/workflows/wheels.yml` runs on the tag `v*`:

1. `build`: one abi3 wheel for each of Linux x86_64, Linux aarch64, macOS arm64, macOS x86_64 and
   Windows x86_64, with a smoke test and the Python tests where the runner allows.
2. `sdist`: `maturin sdist`.
3. `cli`: the standalone program (`cargo build --release --locked -p fastwarp-cli --target
   <triple>`) for the same five platforms, packed as `fastwarp-<triple>.tar.gz` (`.zip` on
   Windows) with `LICENSE`, `NOTICE` and `README.txt`, and a `.sha256` file per archive. Linux
   builds in the pinned manylinux 2.28 image of the wheels (rustup-init checked against a pinned
   SHA-256) and checks that the program needs glibc 2.28 at most and links only the C library,
   `libm`, `libdl`, `libpthread`, `librt` and `libgcc_s` (Vulkan is loaded at run time). macOS
   checks that only system libraries are linked; Windows links the static C runtime and checks
   that no Visual C++ runtime DLL is imported. Each program runs `--version`, `recipe` and
   `gpu-info` (status 0 or 4); the macOS x86_64 program runs only if the runner has Rosetta 2,
   otherwise the job warns. Every leg except macOS x86_64 then installs and uninstalls its archive
   with `scripts/install.sh` or `scripts/install.ps1`, and the macOS arm64 leg runs
   `ci/test-install.sh`.
4. `installers`: after `cli`, checks the `.sha256` files of the archives and writes the version
   of `Cargo.toml` and the SHA-256 of every archive into `scripts/install.sh` and
   `scripts/install.ps1` as `fastwarp-installer.sh` and `fastwarp-installer.ps1`. The installer
   of a release thus installs that release and checks the download against the checksum it
   holds, not against a file next to the archive. It then installs and uninstalls the Linux
   x86_64 archive with the stamped `fastwarp-installer.sh`. On a tag it fails unless the tag
   equals the `Cargo.toml` version.
5. `attest`: build provenance attestations (`actions/attest`) for the wheels, the sdist, the
   archives and the installers. It is the only job with `attestations: write`.
6. `release`: fails unless the tag equals the `Cargo.toml` version, then creates the GitHub
   release as a draft with the wheels, the sdist, the archives with their `.sha256` files, the
   installers and `SHA256SUMS` (every file except the `.sha256` files). The notes are the
   `CHANGELOG.md` section plus a link to the install instructions in `docs/cli.md`.
7. `publish`: after the jobs above succeed, uploads the wheels and the sdist to PyPI
   through OIDC trusted publishing, in the `pypi` environment. Only `attest`, `publish` and
   `publish-testpypi` have `id-token: write`.
8. `publish-release`: after the upload to PyPI succeeds, publishes the drafted GitHub release.
   A failure in any job above leaves the release a draft and PyPI untouched.

If `publish` fails, the GitHub release stays a draft. Fix the cause and use *Re-run failed jobs*
on the same run; files that PyPI already has are skipped. Do not use *Re-run all jobs*:
`release` fails when the draft already exists. A re-run uses the workflow file of the tagged
commit; if the fix is in `wheels.yml`, delete the draft and release the next version.

A PyPI version cannot be replaced once uploaded. If a release is wrong, yank it on PyPI and
release the next version.

`gh workflow run wheels` builds the wheels, the programs and the installers by hand (download
them from the run's artifacts); the `attest`, `release`, `publish` and `publish-release` jobs run
only for a pushed tag, never for a manual run.

The installer scripts are tested on every push (`ci.yml`, job `installers`: ShellCheck,
PSScriptAnalyzer and an end-to-end test of `install.sh` against a fake release) and on ready pull
requests and nightly (`full.yml`, job `installer-windows`: `install.ps1` on Windows PowerShell 5.1
and PowerShell 7). The pinned tools (manylinux image, rustup-init, ShellCheck, PSScriptAnalyzer)
are not updated by Dependabot. Before each release, check them for newer versions and update the
versions and checksums by hand.

## After the release

Check the project page at <https://pypi.org/project/fastwarp/> and install the wheel in a clean
environment: `python -c "import fastwarp; print(fastwarp.__version__)"`.

Check the standalone program on Linux, macOS and Windows with the installer of the release, as a
user would (see [docs/cli.md](docs/cli.md#install)):

```bash
v=<version>
gh release download "v$v" -R WyssCenter/fastwarp -p fastwarp-installer.sh
gh attestation verify fastwarp-installer.sh --repo WyssCenter/fastwarp \
    --signer-workflow WyssCenter/fastwarp/.github/workflows/wheels.yml
sh fastwarp-installer.sh --no-modify-path --dir "$PWD/bin"
bin/fastwarp --version && bin/fastwarp gpu-info
sh fastwarp-installer.sh --uninstall --dir "$PWD/bin"
```

On Windows, the same with `fastwarp-installer.ps1 -NoModifyPath -Dir $PWD\bin` and `-Uninstall`.
`gh attestation verify <file> --repo WyssCenter/fastwarp --signer-workflow WyssCenter/fastwarp/.github/workflows/wheels.yml`
checks any file of the release.
