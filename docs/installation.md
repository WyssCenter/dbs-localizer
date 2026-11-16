# Installation

Every release on the [Releases page](https://github.com/WyssCenter/dbs-localizer/releases) has a
package for each supported system. Each package contains the desktop application
`dbs-localizer`, the command-line tool `dbsloc` and everything they need: the registration tools
(ANTs), the DICOM converter (dcm2niix), a brain template and atlases. Each file is published with
a `.sha256` file that holds its checksum.

| File | System |
| --- | --- |
| `dbs-localizer-setup.exe` | Windows 10 or 11, 64-bit |
| `dbs-localizer_<version>_amd64.deb` | Debian 13 or newer, Ubuntu 24.04 or newer (x86_64) |
| `dbsloc-linux.tar.gz` | other 64-bit (x86_64) Linux distributions, for example Arch or Fedora |
| `dbsloc-mac.tar.gz` | macOS on Apple silicon (arm64) |

## Installing a package

```{include} ../README.md
:start-after: <!-- docs:install -->
:end-before: <!-- /docs:install -->
```

## Installing from the command line

Without a browser download, `install.sh` can fetch the archive itself (Linux and macOS). Download
the script, read it, then run it:

```bash
curl -fsSLO https://raw.githubusercontent.com/WyssCenter/dbs-localizer/main/scripts/install.sh
bash install.sh              # the latest release
bash install.sh -v v0.1.0    # a given release
```

The script downloads the archive and its `.sha256` file from the release and installs only when
the checksum matches. It uses the GitHub CLI when that is installed and signed in, a token in
`GH_TOKEN` (or `GITHUB_TOKEN`) when one is set, and a plain download otherwise.

- **Archive with checksum:** `bash install.sh -a dbsloc-linux.tar.gz` installs a downloaded
  archive after checking the `.sha256` beside it.
- **Another folder:** `-d <folder>` installs into that folder instead of `~/.local`.

`install.sh` checks the CPU architecture (x86_64 on Linux, arm64 on macOS), extracts the new
release into a staging folder, checks that its programs run there, and only then replaces an
installed release.

## Checking a download

Every release lists the SHA-256 of its files in `SHA256SUMS` (and per file in `<file>.sha256`).
In the folder with the downloads:

```bash
sha256sum -c --ignore-missing SHA256SUMS
```

On macOS, print the checksum with `shasum -a 256 dbsloc-mac.tar.gz`; on Windows with
`(Get-FileHash dbs-localizer-setup.exe).Hash.ToLower()`; then compare it with the line in
`SHA256SUMS`.

A checksum shows that the download is complete. The build provenance attestation also shows that
the file was built by the release workflow of this repository from the tag. With the
[GitHub CLI](https://cli.github.com/) (replace `vX.Y.Z` with the release):

```bash
gh attestation verify dbs-localizer-setup.exe --repo WyssCenter/dbs-localizer \
  --signer-workflow WyssCenter/dbs-localizer/.github/workflows/release.yml \
  --source-ref refs/tags/vX.Y.Z
```

The same command works for the `.deb` and the `.tar.gz` archives.

## Where the files go

| Package | Program files | Commands |
| --- | --- | --- |
| Windows installer | `%LOCALAPPDATA%\Programs\Wyss DBS Localizer` (no administrator rights needed) | the install folder is added to your user `PATH`; Start menu and desktop shortcuts |
| Debian package | `/opt/dbs-localizer` | `/usr/bin`; menu entry and icon |
| Linux and macOS archives with `install.sh` | `~/.local/lib/dbsloc` | launchers in `~/.local/bin`; on Linux also a menu entry and icon |

To remove the software: on Windows under *Settings → Apps*; the Debian package with
`sudo apt remove dbs-localizer`; an archive installation with `install.sh -u`.

Settings, logs, downloads and caches are kept in your user folders, not in the program folder;
[Outputs and file layout](outputs.md#files-outside-the-dataset) lists them.

Every package carries `LICENSE`, `NOTICE`, the `LICENSES/` folder and `THIRD_PARTY_LICENSES.md`
(the licences of the Rust libraries built into the programs), and names the tag of the public
repository it was built from.

## From source

Building from source needs Git, Rust (the version is pinned in `rust-toolchain.toml`) and the
C/C++ build tools of your system. The source tree contains the bundled programs and data under
`resources/` and finds them on its own, so no download is needed. The
[contributing guide](contributing.md#development-setup) lists the tools for each system, the
build commands, the build profiles and how to build an archive like the release downloads; the
[licence page](licence.md) explains the build without the optional PaCER component.
