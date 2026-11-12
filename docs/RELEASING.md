# Releasing Wyss DBS Localizer

Releases are driven by git tags. Pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`: it creates a **draft** GitHub release with the notes of the version's section in `CHANGELOG.md`, builds and tests the Windows installer and the Linux packages in one job per platform, and a final job uploads each with its SHA-256 checksum to the draft and publishes it, only when every platform has succeeded (a failed platform leaves the draft unpublished, so `releases/latest` never points at an incomplete release). The build jobs hold a read-only token; only the jobs that create, fill and publish the draft can write to the repository. Nothing is published from a developer machine. Releases and their downloads are public (see *Installing a release* below).

## One version, recorded in three places

- `Cargo.toml` → `[workspace.package] version` (every crate inherits it with `version.workspace = true`; `dbsloc --version`, the GUI's F1 window, the session log, the method logs and `meta.software` in the reconstruction file all read it through `dbsloc_pipeline::version`).
- `pyproject.toml` → `version` (developer tooling only).
- The git tag `vX.Y.Z`.

`scripts/check_release_version.py` refuses a release whose tag disagrees with the two files, and `scripts/release_notes.py` refuses one without a dated `## [X.Y.Z] - YYYY-MM-DD` section in `CHANGELOG.md` (both in the first job of the release workflow, before anything is built). The build also embeds the short git hash (`DBSLOC_GIT_HASH`, from `crates/dbsloc-pipeline/build.rs`; `-dirty` when tracked files had uncommitted changes the last time the build script ran; `scripts/package_release.py` stamps the exact state; other local builds refresh the stamp only when HEAD or the checked-out branch ref changes, so set `DBSLOC_GIT_HASH` to force a value), shown as `0.2.0+a1b2c3d4e5f6` by `dbsloc --version` and stored in every reconstruction file.

## Cutting a release

1. Make sure `main` is green (`Check` on the last commit and a green `Full` run: start it with *Run workflow* when the nightly run has not covered that commit), the dated change log in `docs/PHASE_HISTORY.md` describes what ships, and `CHANGELOG.md` lists the user-facing changes under `## [Unreleased]` ([Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/): `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`; written for users, not a copy of the commit log).
2. Bump the version and refresh the lock files:

   ```bash
   uv run python scripts/bump_version.py 0.2.0
   ```

   The script turns `## [Unreleased]` in `CHANGELOG.md` into `## [0.2.0] - <today>` with a new empty `[Unreleased]` above it and updates the comparison links at the bottom (it refuses when nothing is listed under `[Unreleased]` or the version already has a section), rewrites the two version lines, runs `cargo update --workspace` and `uv lock`, and checks the result. Bumping to the version the files already carry (the first release, `0.1.0`) only dates the changelog section.
3. Commit and tag the same commit, then push both:

   ```bash
   git commit -am "chore(release): v0.2.0"   # includes CHANGELOG.md
   git tag -a v0.2.0 -m "Wyss DBS Localizer v0.2.0"
   git push origin main v0.2.0
   ```

4. Watch the *Release* workflow. On `windows-latest` and `ubuntu-latest` it runs `scripts/package_release.py --no-gui-smoke`: the job builds the release binaries, assembles the folder and archive, extracts it into a temporary folder and tests it with a clean environment (`--version`, bundled template, bundled ANTs and dcm2niix, viewer resolution, GPU probe, a preprocess run; on Windows a PE import check, on Linux an `ldd` check that records the glibc requirement and the system libraries the GUI needs, and that `install.sh` ships executable). Then Windows builds the installer with WiX 7 (`scripts/build_installer.py`) and installs, checks and uninstalls it silently (`--test`); Linux builds the `.deb` (`scripts/build_deb.py`) and installs, checks and removes it with `apt-get` (`--test`). Each platform job keeps its files as a workflow artifact. A failed check fails that job and the release stays a draft (re-run the failed job, or delete the draft and the tag). The *Publish* job runs only when every platform succeeded: it downloads the platforms' artifacts, checks that each holds exactly its own allow-listed files (installer, archive or package plus their `.sha256`, no other file, no artifact from a platform outside this run) and that the checksums match, uploads them to the draft (replacing files of the same name, so a re-run is safe) and publishes it.
5. The release notes are the version's `CHANGELOG.md` section. Fix a mistake in `CHANGELOG.md` on `main` and copy it into the release on GitHub (*Edit release*); the tag stays where it is.

To test the packaging jobs without publishing, run the workflow by hand from the Actions tab (`workflow_dispatch`, or `gh workflow run release.yml --ref main`): the installer and packages appear as workflow artifacts, the release-notes check is skipped and no release is created.

**macOS** is built only on request (where runner minutes are billed, macOS minutes count ten times): the macOS archive, the macOS test job and the macOS clippy job run only when the repository variable `DBSLOC_MACOS` is `true` (*Settings → Secrets and variables → Actions → Variables*, or `gh variable set DBSLOC_MACOS --body true`). The macOS code paths in `package_release.py` and `install.sh` are kept.

## Release assets and where they run

| Asset | Built on | Runs on |
| ----- | -------- | ------- |
| `dbs-localizer-setup.exe` | `windows-latest` (MSVC; WiX 7 Burn bundle around a per-user MSI, `packaging/wix/`) | Windows 10 / 11 x86_64. Shows the licence, installs for the current user under `%LOCALAPPDATA%\Programs\Wyss DBS Localizer` without administrator rights, adds Start menu and desktop shortcuts and the install folder to the user's `PATH` (`dbsloc` in a new terminal); *Settings → Apps* uninstalls it. SmartScreen warns because the binaries are not signed (*More info → Run anyway*). Built from the same folder as the portable `dbsloc-win.zip`, which the job builds and tests but neither publishes nor keeps as a workflow artifact. |
| `dbs-localizer_<version>_amd64.deb` | `ubuntu-latest` | Debian / Ubuntu x86_64 with the runner's glibc or newer (Ubuntu 24.04+, Debian 13+): `sudo apt install ./dbs-localizer_<version>_amd64.deb` also installs the GUI's system libraries. Files under `/opt/dbs-localizer`, commands in `/usr/bin`, menu entry and icon. `Depends` comes from the ELF `NEEDED` entries plus the libraries loaded at run time (`scripts/build_deb.py`). |
| `dbsloc-linux.tar.gz` | `ubuntu-latest` (glibc) | any x86_64 Linux with a glibc at least as new as the runner's (Arch, Fedora, Ubuntu 24.04+, …). Extract and run `./dbsloc-linux/install.sh`: installs under `~/.local` (launchers in `~/.local/bin`, menu entry and icon) without network; `install.sh -u` removes it. The GUI needs `gtk3`, `vulkan-icd-loader`, `libxkbcommon` and `fontconfig` (Arch package names; `install.sh` prints the `pacman` / `apt-get` / `dnf` line when `ldd` finds a missing library). The command line tool runs without them. |
| `dbsloc-mac.tar.gz` | `macos-latest` (Apple silicon), only with `DBSLOC_MACOS` | macOS arm64; not notarised (README.txt explains the quarantine flag; `install.sh` clears it) |

`install.sh` runs the binaries in a staging folder first and only then swaps the installed copy.

Every package folder carries `THIRD_PARTY_LICENSES.md`, generated by `scripts/third_party_licenses.py` from `cargo metadata` (the Rust libraries linked into `dbs-localizer` and `dbsloc`, with their licence texts), beside `LICENSE`, `NOTICE` and `LICENSES/`; `package_release.py` writes it, and the installer and the `.deb` take it from the package folder.

## Installing a release

Download from the [Releases page](https://github.com/WyssCenter/dbs-localizer/releases).

- **Windows:** run `dbs-localizer-setup.exe`.
- **Debian / Ubuntu:** `sudo apt install ./dbs-localizer_<version>_amd64.deb`.
- **Other Linux:** `tar -xzf dbsloc-linux.tar.gz && ./dbsloc-linux/install.sh`.

Without a browser download, `install.sh` can fetch the archive itself (Linux and macOS). Download the script, read it, then run it:

```bash
curl -fsSLO https://raw.githubusercontent.com/WyssCenter/dbs-localizer/main/scripts/install.sh
bash install.sh              # the latest release
bash install.sh -v v0.1.0    # a given release
```

The script downloads the archive and its `.sha256` file from the release and installs only when the checksum matches. It uses the GitHub CLI when that is installed and signed in, a token in `GH_TOKEN` (or `GITHUB_TOKEN`) when one is set, and a plain download otherwise.

- **Archive with checksum:** `bash install.sh -a dbsloc-linux.tar.gz` installs a downloaded archive after checking the `.sha256` beside it.

## Checklist before tagging

- [ ] `uv run python scripts/package_release.py` passes locally on the platform you can test (add `--e2e` for the synthetic end-to-end run); on Windows also `uv run python scripts/build_installer.py --test` (needs WiX 7: `dotnet tool install --global wix --version 7.0.0`, `wix eula accept wix7`, `wix extension add -g WixToolset.BootstrapperApplications.wixext/7.0.0`).
- [ ] `AGENTS.md` §12 and `docs/PHASE_HISTORY.md` are current.
- [ ] Bundled data licences are listed in `NOTICE` (templates, AHEAD, the Lead-DBS atlas folders under `resources/atlases/leaddbs/`, ANTs, dcm2niix).
- [ ] `CHANGELOG.md`: user-facing changes listed under `[Unreleased]` before the bump; after the bump `uv run python scripts/release_notes.py X.Y.Z` prints the section.
- [ ] Version bumped with `scripts/bump_version.py`; `git status` clean before tagging (a dirty tree stamps `-dirty` into the binaries).
