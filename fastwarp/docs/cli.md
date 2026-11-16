# Command line

The `fastwarp` command runs the registration on NIfTI files without writing Python. It comes
with the Python package (after `pip install fastwarp`, both `fastwarp ...` and
`python -m fastwarp ...` work) and, from release 0.2.0, as a standalone program that needs no
Python (see [Install](#install)). All of them run the same code as the Python functions, so
options, results and exit status are the same as described here. Each command below names the
Python function it runs; the [user guide](user-guide.md) explains those functions in more
detail.

```bash
fastwarp register template.nii.gz sub_T1w.nii.gz -o out/sub_
fastwarp apply -r template.nii.gz -i sub_T1w.nii.gz -o out/sub_in_template.nii.gz \
    -t out/sub_Composite.h5
```

`fastwarp --help` lists the commands; `fastwarp <command> --help` lists the options of one
command. `fastwarp --version` prints the version.

## Install

### With the Python package

::::{tab-set}
:sync-group: installer

:::{tab-item} pip
:sync: pip

Into the current Python environment:

```bash
pip install fastwarp
fastwarp --help
```
:::

:::{tab-item} uv
:sync: uv

Into an environment of its own, with `fastwarp` on PATH:

```bash
uv tool install fastwarp
fastwarp --help
```

Without installing:

```bash
uvx fastwarp --help
```
:::

::::

These need Python 3.11 or later. `uv tool install` puts `fastwarp` in uv's tool folder
(`~/.local/bin`, on Windows `%USERPROFILE%\.local\bin`); `uv tool uninstall fastwarp` removes it.

The command line needs release 0.2.0 or later; release 0.1.0 does not have it. To run the
development version, install FastWarp [from source](installation.md#from-source), or run the
command from source without installing it:

```bash
uvx --from "git+https://github.com/WyssCenter/fastwarp.git" fastwarp --help
```

### The standalone program

From release 0.2.0, each [GitHub release](https://github.com/WyssCenter/fastwarp/releases)
carries the program for:

| Archive | System |
| --- | --- |
| `fastwarp-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64, glibc 2.28 or later (the same baseline as the wheels) |
| `fastwarp-aarch64-unknown-linux-gnu.tar.gz` | Linux aarch64, glibc 2.28 or later |
| `fastwarp-aarch64-apple-darwin.tar.gz` | macOS 11 or later, Apple silicon |
| `fastwarp-x86_64-apple-darwin.tar.gz` | macOS 11 or later, Intel |
| `fastwarp-x86_64-pc-windows-msvc.zip` | Windows 10 or later, x86_64; needs no Visual C++ runtime |

Each archive holds `fastwarp` (`fastwarp.exe`), `LICENSE`, `NOTICE` and a `README.txt`, and has
a `.sha256` file beside it. The program loads the GPU driver (Vulkan, Metal or DirectX 12) when
it runs; it needs no other library. The release also carries two installer scripts,
`fastwarp-installer.sh` (Linux, macOS) and `fastwarp-installer.ps1` (Windows), which pick the
archive for the machine, check its SHA-256 and install it. The installer of release X.Y.Z
installs release X.Y.Z and holds the SHA-256 of its archives, so a replaced archive is refused
even when its `.sha256` file was replaced too.

#### Inspect, then run (recommended)

Download the installer, read it, check that the release workflow of this repository built it,
then run it. The check needs the [GitHub CLI](https://cli.github.com/), signed in;
`--signer-workflow` makes it accept only files signed by the release workflow `wheels.yml`.

::::{tab-set}
:sync-group: os

:::{tab-item} Linux, macOS
:sync: unix

```bash
curl --proto '=https' --tlsv1.2 -LsSfO https://github.com/WyssCenter/fastwarp/releases/download/v0.2.0/fastwarp-installer.sh
gh attestation verify fastwarp-installer.sh --repo WyssCenter/fastwarp \
    --signer-workflow WyssCenter/fastwarp/.github/workflows/wheels.yml
less fastwarp-installer.sh
sh fastwarp-installer.sh
```
:::

:::{tab-item} Windows
:sync: windows

```powershell
Invoke-WebRequest -UseBasicParsing https://github.com/WyssCenter/fastwarp/releases/download/v0.2.0/fastwarp-installer.ps1 -OutFile fastwarp-installer.ps1
gh attestation verify fastwarp-installer.ps1 --repo WyssCenter/fastwarp `
    --signer-workflow WyssCenter/fastwarp/.github/workflows/wheels.yml
notepad fastwarp-installer.ps1
powershell -ExecutionPolicy ByPass -File fastwarp-installer.ps1
```
:::

::::

#### One line

The same installers, piped straight into the shell:

:::{caution}
The one-line form trusts the download without reading or checking it first. Prefer
[Inspect, then run](#inspect-then-run-recommended).
:::

::::{tab-set}
:sync-group: os

:::{tab-item} Linux, macOS
:sync: unix

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/WyssCenter/fastwarp/releases/download/v0.2.0/fastwarp-installer.sh | sh
```
:::

:::{tab-item} Windows
:sync: windows

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/WyssCenter/fastwarp/releases/download/v0.2.0/fastwarp-installer.ps1 | iex"
```
:::

::::

For another release, change the version in the address. Options go after `sh -s --`
in the piped form (`... | sh -s -- --no-modify-path`); in PowerShell, use the script-block form
(`& ([scriptblock]::Create((irm <address>))) -NoModifyPath`) or the environment variables below.

#### What the installer does

1. Picks the archive for the operating system and processor, and stops on any other system
   (32-bit, Windows on ARM, musl-based Linux such as Alpine, glibc older than 2.28, macOS older
   than 11).
2. Downloads the archive over HTTPS from the release and compares its SHA-256 with the one the
   installer holds (for a version other than its own, with the release's `.sha256` file); it
   stops without installing anything when they differ.
3. Runs the new program once (`fastwarp --version`) and only then puts it in place. It replaces
   a `fastwarp` already in the folder only if it installed that one itself (see the receipt
   below).
4. Adds the install folder to PATH, unless it is there already or you opt out (below).
5. Writes a receipt, `.fastwarp-installer`, next to the program: the SHA-256 of the program it
   installed and the PATH changes it made. Reinstalling and uninstalling read it.

It needs no administrator rights and never calls `sudo`.

| | Linux, macOS (`fastwarp-installer.sh`) | Windows (`fastwarp-installer.ps1`) |
| --- | --- | --- |
| Install folder | `$XDG_BIN_HOME` if set, else `~/.local/bin` | `%LOCALAPPDATA%\Programs\fastwarp\bin` |
| Other folder | `--dir DIR` or `FASTWARP_INSTALL_DIR` | `-Dir DIR` or `FASTWARP_INSTALL_DIR` |
| Release | `--version X.Y.Z` or `FASTWARP_VERSION` | `-Version X.Y.Z` or `FASTWARP_VERSION` |
| Do not change PATH | `--no-modify-path` or `FASTWARP_NO_MODIFY_PATH=1` | `-NoModifyPath` or `FASTWARP_NO_MODIFY_PATH=1` |
| Uninstall | `--uninstall` | `-Uninstall` |
| Help | `--help` | `-Help` |

`FASTWARP_DOWNLOAD_URL` makes the installer download from another folder that holds the same
file names, for example a mirror: `https://` or `file://`; `http://` only for the installer's own
version, whose checksums it holds, or from `localhost`. Redirects must stay on HTTPS: the
shell installer uses `curl` with `--proto-redir =https`, and with only `wget` (which may follow a
redirect to plain HTTP) it installs only its own version, whose checksum it holds. The install
folder must not contain `:`, `\` or a line break (Linux, macOS) or `;` (Windows), which would
break PATH.

The installer does not replace or remove a `fastwarp` it did not install: one without a matching
receipt, such as a symbolic link from `uv tool install`, a script from `pip install --user`, a
program you built, or one changed since the install. Remove that one first, or install elsewhere
with `--dir`. On Linux and macOS the default folder is the one uv uses for its tools (and pip on
Linux for `pip install --user`). On Windows the program gets a folder of its own, away from
`%USERPROFILE%\.local\bin`, where `uv tool install` puts its own `fastwarp.exe`.

#### PATH

On Linux and macOS, when the install folder is not on PATH, the installer appends one line
ending in `# fastwarp-installer` to `~/.profile`, to `~/.bashrc` when you use bash or the file
exists, to `~/.bash_profile` when it exists, and to `~/.zshrc` (`$ZDOTDIR/.zshrc`) when you use
zsh or the file exists; for fish it adds a line to
`~/.config/fish/conf.d/fastwarp-installer.fish`. The line adds the folder only when it is missing
from PATH, and the installer never writes it twice. It prints the files it changed and records
them in the receipt. Open a new terminal, or run `. ~/.profile`, to use `fastwarp`.

On Windows it adds the folder to the start of the user PATH (`HKCU\Environment`, not the PATH of
the machine), keeps the other entries and their `%VARIABLE%` form, and tells running programs
that the environment changed. Open a new terminal to use `fastwarp`.

With `--no-modify-path` (`-NoModifyPath`) nothing is changed and the installer prints how to add
the folder yourself.

#### Uninstall

::::{tab-set}
:sync-group: os

:::{tab-item} Linux, macOS
:sync: unix

```bash
sh fastwarp-installer.sh --uninstall
```

Add `--dir DIR` if you installed elsewhere.
:::

:::{tab-item} Windows
:sync: windows

```powershell
powershell -ExecutionPolicy ByPass -File fastwarp-installer.ps1 -Uninstall
```
:::

::::

This removes the program, the receipt and exactly the PATH lines (on Windows: the user PATH
entry) that the receipt says the installer added, and then the folder if it is empty. PATH
entries that were there before the install stay. If another tool replaced the program since
the install, that program stays and the installer warns, but its PATH changes are still undone.
A receipt copied from another folder is ignored for PATH changes. When the folder still holds other programs
(for example `~/.local/bin`), the installer removes its PATH lines anyway and says that the
folder is no longer on PATH through them; open a new terminal before installing again, because
the old one still has the folder on PATH. Removing the program by hand works too: delete
`fastwarp`, `.fastwarp-installer` and the lines ending in `# fastwarp-installer` (Linux, macOS),
or the folder `%LOCALAPPDATA%\Programs\fastwarp` and its user PATH entry (Windows).

#### By hand

```bash
gh release download v0.2.0 -R WyssCenter/fastwarp -p "fastwarp-x86_64-unknown-linux-gnu.tar.gz*"
sha256sum -c fastwarp-x86_64-unknown-linux-gnu.tar.gz.sha256
gh attestation verify fastwarp-x86_64-unknown-linux-gnu.tar.gz --repo WyssCenter/fastwarp \
    --signer-workflow WyssCenter/fastwarp/.github/workflows/wheels.yml
tar -xzf fastwarp-x86_64-unknown-linux-gnu.tar.gz
cp fastwarp-x86_64-unknown-linux-gnu/fastwarp ~/.local/bin/
```

On macOS, check the checksum with `shasum -a 256 -c` instead of `sha256sum -c`. `SHA256SUMS` in
the release lists the SHA-256 of every file of the release.

:::{note}
The programs are not code-signed. macOS refuses to open a program from an archive downloaded
with a web browser until you remove the quarantine flag
(`xattr -d com.apple.quarantine fastwarp`), and Windows SmartScreen may warn about
`fastwarp.exe` downloaded with a browser. Downloads made with `curl`, `gh` or the installers are
not flagged.
:::

## Images

The command line reads and writes NIfTI-1 images (`.nii` and `.nii.gz`, the `.gz` in lower
case) only. A file with another extension (`.mha`, `.nrrd`, Analyze `.img`/`.hdr`) is refused
with exit status 2; convert it first. A `.nii` file that is not single-file NIfTI-1 (for example
NIfTI-2) cannot be read and gives exit status 3.

Images must be 3-D. A 4-D file with more than one volume is refused.

An image is read as ITK reads it, and so as ANTsPy's `ants.image_read` reads it:

- the geometry (origin, spacing, direction) comes from the sform or the qform by ITK's rules and
  is used in ITK's LPS convention;
- `scl_slope` and `scl_inter` are applied;
- the voxel values are rounded to `float32`, as an `ANTsImage` of type `float` holds them.

Registering the files with the command line therefore gives the same result as reading them with
`ants.image_read` and passing them to the Python functions with the same settings; note that the
default grid spacing of `register` differs from that of `register_syn` (see below). The tests
check this (`tests/test_cli.py`).

Output images are written as `float32` NIfTI files with both sform and qform set. An output
whose name ends in `.gz` is compressed. The folder of an output file or prefix is created when
it does not exist.

## Commands

### register

```bash
fastwarp register FIXED MOVING -o PREFIX [--fixed2 F2 --moving2 M2] [--iso-mm MM]
                  [--recipe FILE] [--outputs composite|split|both] [--write-warped]
                  [--adapter NAME] [--json]
```

Registers MOVING to FIXED: {term}`centre of mass`, Rigid and Affine
({term}`Mattes mutual information`, first channel), then {term}`SyN` ({term}`cross-correlation`,
every channel), or the start and stages a [stage-list recipe](guide/recipes.md#stage-lists)
names. This is {func}`fastwarp.register_syn`; see [Normalization](guide/normalization.md) and
[Output files](guide/outputs.md).

- `-o, --output PREFIX`: writes `<PREFIX>Composite.h5` (`[Affine, Warp]`),
  `<PREFIX>InverseComposite.h5` (`[InverseWarp, Affine^-1]`) and `<PREFIX>0GenericAffine.mat`.
  A prefix that ends in a path separator, such as `out/sub/`, is a folder, as in ANTs.
- `--outputs`: `composite` (default, the files above), `split` (what `antsRegistration` writes
  without `--write-composite-transform`: `<PREFIX>0GenericAffine.mat` and, after a SyN stage,
  `<PREFIX>1Warp.nii.gz` and `<PREFIX>1InverseWarp.nii.gz`, written as `float32` where ANTs
  writes `float64`) or `both`. Without a SyN stage the
  composites hold only the affine.
- `--fixed2 F2 --moving2 M2`: a second channel (for example T2w) for the SyN stage. The second
  moving image must already be in the space of MOVING. Give both or neither.
- `--iso-mm MM`: spacing of the isotropic grid the registration runs on, in millimetres. The
  default is the smallest voxel spacing of FIXED, so that the registration keeps the detail of
  the fixed image: a 1 mm template registers at 1 mm, a 0.5 mm template at 0.5 mm.

  This default differs from the Python function: `register_syn` uses 1.0 mm unless `iso_mm` is
  given. To get the result of a Python call that leaves `iso_mm` out, pass `--iso-mm 1`. A finer
  grid needs more GPU memory and time; `fastwarp fits` tells whether it fits.
- `--recipe FILE`: the schedule as a JSON file, in the form `fastwarp recipe` prints or in the
  stage-list form of the Python API, for example a SyN-only run from the identity:

  ```json
  {"initial": "identity", "stages": [{"transform": "syn", "cc_radius": 2}], "winsorize": null}
  ```

  `initial` is `"com"` (the default, also for `null`; `antsRegistration` without `-r` starts from
  the identity instead), `"identity"`, `{"path": "start.mat", "invert": false}` (a relative path
  is read from the folder of the recipe file) or `{"parameters": [...], "center": [...]}`;
  `stages` lists
  `rigid` / `affine` stages and at most one `syn` stage, last; `winsorize` is `[lower, upper]`
  or `null` (ANTs' plain rescale to [0, 1]). A stage or field left out keeps the default value.
  An unknown key, a wrong type, an unsupported stage order or a value the GPU kernels do not
  support is refused with exit status 2 before any work.
- `--write-warped`: also writes `<PREFIX>_Warped.nii.gz`, MOVING on the registration grid.
- `--adapter NAME`: use the GPU whose name contains NAME.
- `--json`: print the timings and diagnostics as JSON: the keys of the dictionary
  `register_syn` returns (`files` among them), with `levels` as a list of objects, and
  `outputs`, the files written (the same list as `files`).

The capacity check runs before the registration: a grid that does not fit the GPU stops with
exit status 5, and no transform files are written (the output folder may already have been
created). It checks the linear stages and the SyN stage each on its own and requires the larger
of the two to fit.

### linear

```bash
fastwarp linear FIXED MOVING [-o PREFIX] [--transform rigid|affine] [--mask MASK]
                [--winsorize LOWER,UPPER] [--initial FILE] [--stage FILE]
                [--adapter NAME] [--json]
```

One rigid or affine fit of MOVING to FIXED (Mattes mutual information, regular sampling). This
is {func}`fastwarp.linear_session` with one {meth}`~fastwarp.LinearSession.register` call; see
[Linear fits](guide/linear.md).

- `-o, --output PREFIX`: writes `<PREFIX>0GenericAffine.mat`. Without it nothing is written and
  the result is only printed.
- `--transform`: `rigid` (default) or `affine`.
- `--mask MASK`: a fixed-image mask on any grid; the metric samples only its non-zero voxels.
- `--winsorize LOWER,UPPER`: intensity quantiles, for example `0.005,0.995`, as ANTs'
  `--winsorize-image-intensities`.
- `--initial FILE`: start from this affine transform (`.mat` or `.txt`, one affine that maps
  fixed points to moving points, such as a `0GenericAffine.mat`). Without it the fit starts
  from the centres of intensity mass, like `--initial-moving-transform [fixed,moving,1]`.
- `--stage FILE`: the schedule as a JSON file with the fields of the `rigid` entry of
  `fastwarp recipe`. Fields left out keep the default.
- `--json`: print the result as JSON: the keys `LinearSession.register` returns
  (`parameters`, `center`, `metric`, `iterations`, ...) and `outputs`.

### apply

```bash
fastwarp apply -r REFERENCE -i INPUT -o OUTPUT -t T1 [-t T2 ...]
               [-n linear|nearestNeighbor|lanczosWindowedSinc] [-f VALUE]
```

Resamples INPUT onto the grid of REFERENCE through the transforms, as `antsApplyTransforms`
does. This is {func}`fastwarp.apply_transforms`; it runs on the CPU and needs no GPU.

- `-t, --transform FILE`: a transform file: `.h5` composite, `.mat` or `.txt` affine, or a
  `.nii`/`.nii.gz` displacement field. Repeat it for several. The order is that of
  `antsApplyTransforms -t` and of the list `ants.apply_transforms` takes: a point of the
  reference grid goes through the first transform given first, then through the next. For the
  output of `fastwarp register`, give `-t <PREFIX>Composite.h5` to bring MOVING onto FIXED, and
  `-t <PREFIX>InverseComposite.h5` to bring FIXED onto MOVING. `-t [FILE,1]` uses the inverse of
  a linear transform (`.mat`, `.txt`, or a composite of affines) in its place, as in ANTs
  (quote it in a shell that expands brackets); with the split outputs FIXED goes onto
  MOVING with `-t [<PREFIX>0GenericAffine.mat,1] -t <PREFIX>1InverseWarp.nii.gz`. Inverting a
  displacement field is refused with exit status 2 (ANTs exits 1 without a message): pass the
  matching `InverseWarp` / `InverseComposite` file.
- `-n, --interpolation`: `linear` (default), `nearestNeighbor` or `lanczosWindowedSinc`.
- `-f, --default-value`: value of points that map outside INPUT (default 0).

The output is a `float32` image with the grid of REFERENCE.

### compose

```bash
fastwarp compose -r REFERENCE -o FIELD -t T1 [-t T2 ...] [--compression-level N]
```

Writes the transforms as one displacement field on the grid of REFERENCE, the file
`antsApplyTransforms --compose` writes (`float32` vectors in LPS millimetres). This is
{func}`fastwarp.compose_field`; it needs no GPU. The order of `-t` is as for `apply`.
`--compression-level` (0 to 9) sets the gzip level of a `.nii.gz` output; 0 stores the data
without compressing it, which suits displacement fields.

### recipe

```bash
fastwarp recipe [--channels 1|2]
```

Prints the default [recipe](guide/recipes.md) as JSON, the same dictionary as
{func}`fastwarp.cohort_recipe`. Edit a copy and pass it with `--recipe`.

### gpu-info

```bash
fastwarp gpu-info [--adapter NAME] [--json]
```

Prints the GPU FastWarp would use as JSON (`name`, `backend`, `device_type`, `driver`,
`memory_budget_bytes`), the same as {func}`fastwarp.gpu_info`. Exit status 4 when no usable GPU is
found.

### fits

```bash
fastwarp fits FIXED [MOVING] [--iso-mm MM] [--channels 1|2] [--recipe FILE] [--adapter NAME]
              [--json]
```

Checks whether a registration of FIXED (and MOVING) at `--iso-mm` fits the GPU, with the check
`register` runs first. `--iso-mm` defaults to the smallest voxel spacing of FIXED, as for
`register` (the Python function `fits_gpu` defaults to 1.0 mm). Only the image headers are
read. Exit status 0 when it fits, 5 when it does not, 4 when there is no usable GPU. Without
MOVING, the moving grid is taken to be the fixed grid. `--recipe` checks the stages of that
recipe (default: the default recipe); a chain without a SyN stage needs far less GPU memory. This
is {func}`fastwarp.fits_gpu`; see [GPU and memory](guide/gpu.md#what-fits).

### bench

```bash
fastwarp bench [--size NAME ...] [--repeat N] [--warmup N] [--threads N]
               [--recipe FILE] [--adapter NAME] [--json]
fastwarp bench FIXED MOVING [--iso-mm MM] [--repeat N] ...
```

Times the registration (the `register` path, without writing files) and prints the machine it
ran on. It needs no ANTs and downloads nothing.

Without images it registers built-in synthetic head volumes. They are computed from formulas, so
every run on every machine registers the same data and timings can be compared. The moving
volume is the fixed one deformed smoothly and moved by a small rotation and shift. The sizes
cover the MNI152 field of view (182 x 218 x 182 mm):

| `--size` | Grid | Spacing |
| --- | --- | --- |
| `tiny` | 30 x 36 x 30 | 6 mm (for tests) |
| `mni-2mm` | 91 x 109 x 91 | 2 mm |
| `mni-1mm` | 182 x 218 x 182 | 1 mm |
| `mni-0.5mm` | 364 x 436 x 364 | 0.5 mm |

The default is `mni-1mm` and `mni-0.5mm`. Each size registers at its own spacing. A size that
does not fit the GPU is skipped with a note. A size whose registration fails is reported as
failed and the other sizes still run. The exit status is 1 when a size failed, else 5 when every
size was skipped. Only GPU memory is checked: the `mni-0.5mm` size also needs about 2 GB of host
memory. With FIXED and MOVING, it times your images at `--iso-mm` (default: the smallest voxel
spacing of FIXED, as for `register`).

- `--repeat N`: timed runs per size (default 1).
- `--warmup N`: untimed runs before the timed ones. The default is 1 when `--repeat` is above 1,
  else 0.
- `--threads N`: a cap on the CPU threads FastWarp uses for its host-side work: resampling,
  intensity preprocessing and reading the input images. The GPU work is not limited. The
  default is all logical cores, or `RAYON_NUM_THREADS` when it is set. It must be set before
  anything in the process has started the thread pool, so with `python -m fastwarp` use it in a
  fresh process.
- `--recipe FILE`: a recipe as for `register` (default: the default recipe).
- `--json`: print the report as JSON.

Each run reports four times in seconds:

- `linear_s`: the centre of mass, Rigid and Affine stages;
- `syn_s`: the SyN stage;
- `total_s`: all phases of the registration: resampling to the isotropic grid, intensity
  preprocessing, linear, SyN and the final warp;
- `wall_s`: the whole call, including opening the GPU and the capacity check.

With more than one run, a `med` line gives the median of each column. The machine block lists
the GPU name, driver, backend, device type and memory budget, the CPU model, logical cores and
threads used, the operating system and the FastWarp version.

### ants-registration

```bash
fastwarp ants-registration [antsRegistration arguments]
```

Takes the arguments of `antsRegistration` (ANTs 2.6.5), runs the subset FastWarp supports on
the GPU and writes the files `antsRegistration` writes, under the same names. Only this explicit
subcommand runs it: the program never behaves as `antsRegistration` because of the name it is
called by, so a copy of FastWarp cannot take the place of ANTs on PATH by accident. Every option
that would change the result and is not supported is refused with exit status 2 and a message
that names the option and the reason, before the GPU is opened; options and file names are
checked before any file is read, the images (for example two SyN channels on different fixed
grids) right after they are read. The options this command accepts all take effect, or are
ANTs' defaults. From Python, [`fastwarp.ants`](guide/ants.md) runs the same code on the argument
lists ANTsPy builds.

**Levels without usable samples are skipped, where ANTs drops the whole stage.** When a level of a Rigid or Affine stage gets no sample point on the fixed image, or only points with one fixed value (typically a coarse level on a small image or inside a small mask: at a shrink factor of 6 a 32-voxel axis has 5 voxels, the regular sampling stride with 20 % of the voxels is 5 as well, and every candidate falls on the same mask edge), Mattes mutual information is undefined there. ANTs 2.6.5 then throws inside the stage, catches the exception, drops the whole stage and exits with status 0, so only the start transform remains (ANTsPy returns it without notice). FastWarp skips only that level, keeps the parameters, runs the finer levels and warns on standard error (`fastwarp: warning: ants-registration: linear stage 1
(rigid): level 1 of 4 (shrink factors 6x6x6) was skipped: ...`). When every level of a stage is
skipped, the command fails with exit status 1 and names the remedies (smaller shrink factors, a
larger sampling percentage, a larger mask, a finer registration grid). `fastwarp register` and
`fastwarp linear` skip and warn the same way.

```bash
fastwarp ants-registration -d 3 -r [f.nii.gz,m.nii.gz,1] \
    -t Rigid[0.1] -m MI[f.nii.gz,m.nii.gz,1,32,Regular,0.25] \
    -c [1000x500x250x100,1e-6,10] -f 8x4x2x1 -s 3x2x1x0vox \
    -t Affine[0.1] -m MI[f.nii.gz,m.nii.gz,1,32,Regular,0.25] \
    -c [1000x500x250x100,1e-6,10] -f 8x4x2x1 -s 3x2x1x0vox \
    -t SyN[0.1,3,0] -m CC[f.nii.gz,m.nii.gz,1,4] \
    -c [100x70x50x20,1e-6,10] -f 8x4x2x1 -s 3x2x1x0vox \
    -o [out/sub_,out/sub_Warped.nii.gz,out/sub_InverseWarped.nii.gz]
```

The values are parsed as ANTs parses them: names of transforms, metrics, sampling strategies and
interpolators in any case, brackets split over several shell words (`-c [ 100x70,1e-6,10 ]`, as
the ANTs scripts write them), and the options of a stage in either order (`-t` before `-m`, as
the scripts do, or `-m` before `-t`, as ANTsPy does). Stage options are assigned as ANTs
assigns them: the k-th `-t`, `-c`, `-f` and `-s` belong to stage k, and a `-m` starts the metrics
of the next stage unless the option just before it is a `-m` too. A list whose number of
metric stages, `-c`, `-f` or `-s` differs from the number of `-t` is refused (ANTs exits 1 with
"The number of stages does not match up with the metrics").

Supported:

| Option | Supported values |
| --- | --- |
| `-d` | `3` (may be left out) |
| `-o` | `PREFIX` or `[PREFIX,warped,inverseWarped]`; the prefix is text, as in ANTs |
| `-r` | `[fixed,moving,1]` (centres of intensity mass, on the images of the first metric), `Identity`, one affine file (`.mat`, `.txt`) or `[file,1]` for its inverse |
| `-t` | `Rigid[step]` and `Affine[step]` stages, any number, then at most one `SyN[step,<3>,<0>]` stage; `SyN` alone is allowed |
| `-m` | Rigid and Affine stages: one `MI[fixed,moving,weight,32,Regular,percentage]` (or `Mattes[...]`); SyN stage: one or two `CC[fixed,moving,weight,radius]` with the same radius, 1 to 8 (two metrics are two channels) |
| `-c`, `-f`, `-s` | `MxN` or `[MxN,threshold,window]` (window at least 2), shrink factors, sigmas in `vox` (default) or `mm`; one value per level |
| `-x` | `[fixedMask,NULL]` (`NA`, `NONE` also mean no mask), once for all stages or once per stage; Rigid and Affine stages only |
| `-w` | `[lower,upper]`; without it the images are rescaled to [0, 1] as ANTs does |
| `-a` | `0` or `1` (`Composite.h5` and `InverseComposite.h5` only) |
| `-z`, `-i`, `-u`, `-p`, `--write-interval-volumes`, `--minc` | `1`, `0`, `0`, `0`, `0`, `0` (the defaults) |
| `-g` | all-ones vectors (no restriction) |
| `-n` | `Linear`, `NearestNeighbor`, `LanczosWindowedSinc`: the warped images only |
| `--float` | `0` or `1`: the voxel type of the warped images and warp fields |
| `--random-seed` | the seed of every linear stage; without it `ANTS_RANDOM_SEED`, else 20260910 |
| `-v` | prints the stages, levels, timings and files on standard output |
| `--version`, `-h`, `--help` | print the version or the supported subset |

Refused, with the reason in the message: `-d 2` and `-d 4`; `--save-state`, `--restore-state`;
`-z 0` (per-stage transform files); `-i 1`; `-u 1` (histogram matching) and a bare `-u`; `-q`;
`-r [fixed,moving,0]` and `[...,2]`, a displacement field or more than one `-r`, and centres of
mass of other images than the first metric's; the transforms `Translation`, `Similarity`,
`CompositeAffine`, `BSpline`, `BSplineSyN`, `GaussianDisplacementField`,
`BSplineDisplacementField`, `TimeVaryingVelocityField`, `TimeVaryingBSplineVelocityField`,
`Exponential`, `BSplineExponential`; a linear stage after the SyN stage or two SyN stages; MI in
the SyN stage (the default of ANTsPy's `SyN`, `SyNRA`, `SyNOnly`: use `CC`), CC in a Rigid or
Affine stage, `MeanSquares`, `Demons`, `GC` and the point-set metrics; MI with bins other than
32, with dense sampling (no strategy or `None`, as in `MI[f,m,1,32]`) or `Random` sampling; CC
with a sampling strategy, a radius above 8, two radii, or more than two CC metrics;
`useGradientFilter` 1; two metrics in a linear stage; linear stages on other images than the
first SyN metric's; a fixed mask on the SyN stage and any moving mask; `-g` with a restriction;
`-p` and `--write-interval-volumes` above 0; `--minc 1`; the interpolators `MultiLabel`,
`Gaussian`, `BSpline`, `CosineWindowedSinc`, `WelchWindowedSinc`, `HammingWindowedSinc`,
`GenericLabel`; images other than NIfTI; an unknown option.

Outputs (with `-z 1`, the default):

| Run | Files |
| --- | --- |
| a start (`-r`) or a linear stage, then SyN | `<PREFIX>0GenericAffine.mat`, `<PREFIX>1Warp.nii.gz`, `<PREFIX>1InverseWarp.nii.gz` |
| linear stages only | `<PREFIX>0GenericAffine.mat` |
| SyN only, no `-r` | `<PREFIX>0Warp.nii.gz`, `<PREFIX>0InverseWarp.nii.gz` |
| any, with `-a 1` | `<PREFIX>Composite.h5`, `<PREFIX>InverseComposite.h5` only |

`warped` is the moving image resampled onto the fixed image's grid and `inverseWarped` the fixed
image resampled onto the moving image's grid, from the images as read (not the rescaled ones),
with `-n`; both are written for linear-only runs too. Warped images and warp fields are
`float64`, or `float32` with `--float 1`, as ANTs writes them, with ANTs' header fields. The warp
fields lie on the fixed image's grid, as in ANTs, also when the registration ran on a finer
isotropic grid (N1). The output folders must exist: a missing one is refused with exit status 2
before the GPU is opened, as the other `fastwarp` commands refuse invalid arguments (ANTs runs
the registration and then fails to write). A mask file that does not exist is an error (ANTs
silently registers without the mask). The `fastwarp` exit statuses apply
([Exit status](#exit-status)); a capacity failure (5) or no GPU (4) can fall back to ANTs with
the same arguments.

The result differs from ANTs' in these ways. The tests in `crates/fastwarp-cli/tests/ants_mode.rs`
run both programs on a pair that starts 6 to 7 mm apart (shifted, rotated and bent) and compare
the file names, voxel types and header dimensions, the warped images, the affine at the corners
of the field of view and the forward map inside the object. With an isotropic fixed image the
affines agree to 0.06 to 0.5 mm at the corners (a run that does nothing: 5.6 to 7.5 mm) and the
forward maps to an RMS of 0.03 to 0.06 mm (ANTs' affine without its warp: 0.9 mm); the warped
images have a correlation of 1.000 with ANTs' (unregistered: 0.83). With a fixed image of 2 x 2 x
3 mm voxels the corners differ by 1.1 mm and the forward map by 0.36 mm RMS (N1).

- N1, registration grid: FastWarp registers on an isotropic grid at the fixed image's smallest
  voxel spacing. For an isotropic fixed image this is the fixed grid itself, as in ANTs. For an
  anisotropic one the images are resampled to that grid (its extent may end up to one voxel short
  of the fixed image's along the coarse axes) and every axis gets the same shrink factor. The
  warp fields are then resampled (linearly) onto the fixed image's grid, where ANTs writes them;
  points of that grid beyond the registration grid get a zero displacement.
- N2, moving image: it is resampled to that grid as well (without smoothing first); ANTs samples
  the moving image on its own grid. A moving image much finer than the fixed one loses detail at
  the last level.
- N3, precision: the GPU computes in `float32`, also without `--float`, where ANTs computes in
  `double`. The results differ in the last digits, more after many iterations.
- N4, random sampling: the regular MI sampling uses ITK's random jitter with ITK's seed sequence;
  without `--random-seed` and `ANTS_RANDOM_SEED` FastWarp uses the fixed seed 20260910, where
  ANTs uses the clock. A FastWarp run repeats exactly on the same GPU.
- N6, centres of mass: computed on the images resampled to the registration grid.
- N7, voxel values: warped images and fields hold `float32` values even when written as
  `float64`.
- N8, transform types: `.mat` and `.h5` files name the `double` types
  (`AffineTransform_double_3_3`) also with `--float 1`, where ANTs writes `_float_3_3`; every
  reader accepts both.

N5 (intensity preprocessing) and N9 (warped outputs) of the design list do not apply here: the
intensity rescale (ANTs' `[0, 1]` window, or `-w`) and the warped outputs on the native grids
are the same as in ANTs.

A few calls that ANTs runs are refused because they are almost always mistakes: an empty output
prefix, a bare `-u` (its meaning in ANTs is unclear), `MI[...]` without the bin count, centres
of mass (`-r [fixed,moving,1]`) of other images than the first metric's,
compared by file name, and more than one `-r`.

### ants-apply-transforms

```bash
fastwarp ants-apply-transforms [antsApplyTransforms arguments]
```

Takes the arguments of `antsApplyTransforms` (ANTs 2.6.5) and writes what it writes, on the
CPU, with the code of `fastwarp apply` and `fastwarp compose`. Refusals work as for
`ants-registration`. Options ANTs accepts that change nothing here are accepted with a warning
on standard error: `-z`, `--time-index` (it applies to `-e 3` only) and `-u` with a transform
output (ANTs ignores it there too; the type follows `--float`).

| Option | Supported values |
| --- | --- |
| `-d`, `-e` | `3`, `0` or `scalar` (may be left out) |
| `-i`, `-r` | 3-D NIfTI images |
| `-o` | `warped.nii.gz`; `DisplacementField[field.nii.gz]` or `[field.nii.gz,1]` (the transforms as one field on the reference grid); `[field.h5,1]` (that field as an HDF5 transform); `CompositeTransform[composite.h5]` (the transforms, not collapsed); `Linear[affine.mat,<invert>]` (linear transforms collapsed into one affine, inverted with 1) |
| `-t` | `file` or `[file,1]`, repeated; the order is that of ANTs (see [apply](#apply)); no `-t` is the identity |
| `-n` | `Linear`, `NearestNeighbor`, `LanczosWindowedSinc` |
| `-u` | image output: `char`, `uchar`, `short`, `int`, `float`, `double`, `default` (`double`, or `float` with `--float 1`); field outputs are `double`, or `float` with `--float 1` |
| `-f`, `--float`, `-v` | default value, voxel type, verbose |
| `-z`, `--time-index` | accepted with a warning |

Refused: `-d 2`, `-d 4`; `-e` other than 0 (`scalar`); the interpolators FastWarp does not have
(see above); inverting a displacement field (`-t [1Warp.nii.gz,1]`, where ANTs exits 1 without a
message: pass the matching `InverseWarp`); a `Linear[...]` output with a field;
`CompositeTransform[...]` other than `.h5` and `Linear[...]` other than `.mat`. Integer `-u`
types take each value truncated toward zero and wrapped to the type's width, as ANTs writes them
(`uchar` -2.7 gives 254, `short` 70000.6 gives 4464). A `[field.h5,1]` output holds the field
inside a composite transform, where ANTs writes the field transform alone; both read the same.
The inversion of a `.nii` / `.nii.gz` transform is refused when the arguments are parsed; whether
an `.h5` or `.mat` file holds a field shows only when it is read.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Success. For `fits`: the registration fits. |
| 1 | The registration, or another step of the engine, failed. An internal error prints one line; set `RUST_BACKTRACE=1` to see Rust's panic message as well. |
| 2 | Invalid arguments: an unknown option, a bad value, an unsupported image format, a bad recipe. |
| 3 | A file could not be read or written (missing input, unreadable NIfTI or transform file). |
| 4 | No usable GPU (`GpuUnavailableError` in Python). |
| 5 | The grid does not fit the GPU (`GpuCapacityError` in Python); for `bench`, every size. |
| 6 | The run was cancelled (`RegistrationCancelled` in Python). |

Status 4 and 5 mean that the GPU cannot run this registration, so a script can run ANTs instead
([Fall back to ANTs](guide/gpu.md#fall-back-to-ants)). Errors are printed as one line on
standard error, starting with `fastwarp: error:`. Results go to standard output.

Ctrl-C stops the program at once, as for any other program; files that were being written may be
incomplete. Status 6 is not produced by Ctrl-C in this version; it is reserved for a cancelled
run.

## Environment

The command line reads the same [environment variables](environment.md) as the Python package,
at the moment the GPU is opened:

| Variable | Effect |
| --- | --- |
| `FASTWARP_GPU_BACKEND` | `vulkan`, `metal`, `dx12` or `auto` (default) |
| `FASTWARP_GPU_ADAPTER` | pick the adapter whose name contains this text (`--adapter` wins) |
| `FASTWARP_GPU_ALLOW_SOFTWARE=1` | accept a software adapter (lavapipe, WARP); slow, for tests |
| `FASTWARP_GPU_MEMORY_BUDGET_MB` | override the memory budget of the capacity check |
| `FASTWARP_GPU_WAIT_TIMEOUT_S` | seconds to wait for the GPU before failing (default 300) |
| `RAYON_NUM_THREADS` | CPU threads for the host-side work (default: all logical cores) |

The former `DBSLOC_` names are still read when the `FASTWARP_` name is unset.

## Building the program

The standalone program is built from the crate `fastwarp-cli`:

```bash
cargo build --release --locked -p fastwarp-cli
target/release/fastwarp --help
```

It behaves exactly like `python -m fastwarp` and needs no Python. The release builds add
`--target <triple>`, build the Linux programs in the manylinux 2.28 container of the wheels and
link the Windows program with the static C runtime
(`RUSTFLAGS="-C target-feature=+crt-static"`); see `.github/workflows/wheels.yml`.
