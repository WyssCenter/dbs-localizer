#!/bin/sh
# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0
#
# Installs the fastwarp command line from a GitHub release (Linux and macOS). See --help.

set -eu

INSTALLER_VERSION="@FASTWARP_VERSION@"
SHA256_X86_64_LINUX="@FASTWARP_SHA256_x86_64-unknown-linux-gnu@"
SHA256_AARCH64_LINUX="@FASTWARP_SHA256_aarch64-unknown-linux-gnu@"
SHA256_AARCH64_MACOS="@FASTWARP_SHA256_aarch64-apple-darwin@"
SHA256_X86_64_MACOS="@FASTWARP_SHA256_x86_64-apple-darwin@"
REPO_URL="https://github.com/WyssCenter/fastwarp"
MARKER="# fastwarp-installer"
MIN_GLIBC="2.28"
MIN_MACOS="11.0"
NL='
'

usage() {
    cat <<EOF
fastwarp installer ${INSTALLER_VERSION}

Downloads the fastwarp command line from ${REPO_URL}/releases, checks its
SHA-256 and installs it without administrator rights. The installer of a
release holds the SHA-256 of that release's archives and checks the download
against it; for another version it uses the release's .sha256 file.

Usage: fastwarp-installer.sh [options]

Options:
  --version VERSION  release to install, for example 0.2.0
                     (default: ${INSTALLER_VERSION}, the release this script belongs to)
  --dir DIR          install into DIR (default: \$XDG_BIN_HOME, else ~/.local/bin)
  --no-modify-path   do not add DIR to PATH in your shell profiles
  --uninstall        remove fastwarp from DIR and the PATH lines this installer added
                     (it writes DIR/.fastwarp-installer to know what it added)
  -h, --help         show this help

Environment:
  FASTWARP_VERSION         same as --version
  FASTWARP_INSTALL_DIR     same as --dir
  FASTWARP_NO_MODIFY_PATH  set to 1 for --no-modify-path
  FASTWARP_DOWNLOAD_URL    download from this folder instead of the GitHub
                           release (https:// or file://; http:// only with the
                           checksum held in the installer, or from localhost)

Unless DIR is on PATH already or --no-modify-path is given, the installer adds
DIR to PATH in ~/.profile and in the profile of your shell (~/.bashrc,
~/.bash_profile, ~/.zshrc, fish conf.d). Each line it adds ends in
'${MARKER}'; --uninstall removes them.
EOF
}

say() {
    printf '%s\n' "$*"
}

err() {
    printf 'fastwarp-installer: error: %s\n' "$*" >&2
    exit 1
}

version_ge() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        na = split(a, x, "."); nb = split(b, y, ".");
        n = (na > nb) ? na : nb;
        for (i = 1; i <= n; i++) {
            p = (i <= na) ? x[i] + 0 : 0; q = (i <= nb) ? y[i] + 0 : 0;
            if (p > q) exit 0;
            if (p < q) exit 1;
        }
        exit 0
    }'
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)
            if ldd --version 2>&1 | grep -qi musl; then
                err "musl-based Linux (for example Alpine) is not supported; the release is built for glibc ${MIN_GLIBC} or later"
            fi
            glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}') || glibc=
            [ -n "$glibc" ] || err "could not find the glibc version (getconf GNU_LIBC_VERSION); the release needs glibc ${MIN_GLIBC} or later"
            version_ge "$glibc" "$MIN_GLIBC" || err "glibc $glibc is too old; the release needs glibc ${MIN_GLIBC} or later"
            case "$arch" in
                x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
                aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
                *) err "unsupported CPU architecture on Linux: $arch (supported: x86_64, aarch64)" ;;
            esac
            ;;
        Darwin)
            macos=$(sw_vers -productVersion 2>/dev/null) || macos=
            if [ -n "$macos" ]; then
                version_ge "$macos" "$MIN_MACOS" || err "macOS $macos is too old; the release needs macOS ${MIN_MACOS} or later"
            fi
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                arch=arm64
            fi
            case "$arch" in
                arm64 | aarch64) target=aarch64-apple-darwin ;;
                x86_64) target=x86_64-apple-darwin ;;
                *) err "unsupported CPU architecture on macOS: $arch (supported: arm64, x86_64)" ;;
            esac
            ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            err "this installer is for Linux and macOS; on Windows use fastwarp-installer.ps1"
            ;;
        *)
            err "unsupported operating system: $os (supported: Linux, macOS)"
            ;;
    esac
}

have() {
    command -v "$1" >/dev/null 2>&1
}

download() {
    url=$1
    out=$2
    case "$url" in
        file://*)
            cp "${url#file://}" "$out"
            ;;
        https://*)
            if have curl; then
                curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL --retry 3 -o "$out" "$url"
            elif have wget; then
                wget --secure-protocol=TLSv1_2 -q -O "$out" "$url"
            else
                err "neither curl nor wget is installed"
            fi
            ;;
        http://*)
            if have curl; then
                curl -fsSL -o "$out" "$url"
            elif have wget; then
                wget -q -O "$out" "$url"
            else
                err "neither curl nor wget is installed"
            fi
            ;;
        *)
            err "unsupported download URL: $url"
            ;;
    esac
}

sha256_of() {
    if have sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    elif have shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif have openssl; then
        openssl dgst -sha256 -r "$1" | awk '{print $1}'
    else
        err "no SHA-256 tool found (sha256sum, shasum or openssl); refusing to install an unchecked download"
    fi
}

path_has() {
    case ":${PATH}:" in
        *":$1:"*) return 0 ;;
        *) return 1 ;;
    esac
}

shell_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

profile_files() {
    shell_name=$(basename "${SHELL:-sh}")
    printf '%s\n' "$HOME/.profile"
    if [ "$shell_name" = bash ] || [ -f "$HOME/.bashrc" ]; then
        printf '%s\n' "$HOME/.bashrc"
    fi
    if [ -f "$HOME/.bash_profile" ]; then
        printf '%s\n' "$HOME/.bash_profile"
    fi
    zdot=${ZDOTDIR:-$HOME}
    if { [ "$shell_name" = zsh ] && [ -d "$zdot" ]; } || [ -f "$zdot/.zshrc" ]; then
        printf '%s\n' "$zdot/.zshrc"
    fi
}

fish_file() {
    printf '%s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/fastwarp-installer.fish"
}

wants_fish() {
    [ "$(basename "${SHELL:-sh}")" = fish ] || [ -d "${XDG_CONFIG_HOME:-$HOME/.config}/fish" ]
}

posix_line() {
    q=$(shell_quote "$1")
    # shellcheck disable=SC2016
    printf 'case ":${PATH}:" in *:%s:*) ;; *) export PATH=%s":$PATH" ;; esac %s\n' "$q" "$q" "$MARKER"
}

fish_line() {
    q=$(shell_quote "$1")
    # shellcheck disable=SC2016
    printf 'contains -- %s $PATH; or set -gx PATH %s $PATH %s\n' "$q" "$q" "$MARKER"
}

has_line() {
    [ -f "$1" ] && grep -Fxq -- "$2" "$1"
}

append_line() {
    if [ -s "$1" ] && [ -n "$(tail -c 1 "$1")" ]; then
        printf '\n' >>"$1" 2>/dev/null || return 1
    fi
    printf '%s\n' "$2" >>"$1" 2>/dev/null
}

remove_line() {
    has_line "$1" "$2" || return 1
    kept="$1.fastwarp-installer.tmp"
    if ! printf '' >"$kept"; then
        err "could not create $kept; $1 was left unchanged"
    fi
    grep_status=0
    grep -Fxv -- "$2" "$1" >"$kept" || grep_status=$?
    if [ "$grep_status" -gt 1 ]; then
        rm -f "$kept"
        err "could not edit $1; it was left unchanged"
    fi
    if ! cat "$kept" >"$1"; then
        err "could not write $1; its content without the PATH line is in $kept"
    fi
    rm -f "$kept"
}

receipt_sha256s() {
    [ -f "$receipt" ] || return 0
    awk '$1 == "sha256" {print $2}' "$receipt"
}

receipt_dir() {
    [ -f "$receipt" ] || return 0
    sed -n 's/^dir //p' "$receipt" | head -n 1
}

receipt_entries() {
    [ -f "$receipt" ] || return 0
    grep -E '^(posix|fish):' "$receipt" || true
}

canonical() {
    (cd "$1" 2>/dev/null && pwd -P)
}

is_owned() {
    [ -L "$bin" ] && return 1
    [ -d "$bin" ] && return 1
    owned=$(receipt_sha256s)
    [ -n "$owned" ] || return 1
    current=$(sha256_of "$bin" | tr 'A-F' 'a-f')
    printf '%s\n' "$owned" | grep -Fxq -- "$current"
}

check_owned() {
    if [ -d "$bin" ] && [ ! -L "$bin" ]; then
        err "$bin is a folder, not a program; remove it or choose another folder with --dir"
    fi
    if { [ -e "$bin" ] || [ -L "$bin" ]; } && ! is_owned; then
        err "$bin was not installed by this installer (for example a link from 'uv tool install', a pip script or a program you built); $1"
    fi
}

modify_path() {
    added=
    if [ -z "${HOME:-}" ]; then
        say "HOME is not set, so no shell profile was changed."
        print_path_help
        return
    fi
    if path_has "$dir"; then
        say "$dir is already on PATH; no shell profile was changed."
        return
    fi
    line=$(posix_line "$dir")
    files=$(profile_files)
    old_ifs=$IFS
    IFS=$NL
    set -f
    for f in $files; do
        has_line "$f" "$line" && continue
        entry="posix:$dir:$f"
        write_receipt "$installed_sha" "$previous$NL$added$NL$entry"
        if append_line "$f" "$line"; then
            added="$added$entry$NL"
        else
            say "warning: could not write $f" >&2
        fi
    done
    set +f
    IFS=$old_ifs
    if wants_fish; then
        ff=$(fish_file)
        fl=$(fish_line "$dir")
        if ! has_line "$ff" "$fl"; then
            entry="fish:$dir:$ff"
            write_receipt "$installed_sha" "$previous$NL$added$NL$entry"
            if mkdir -p "$(dirname "$ff")" 2>/dev/null && append_line "$ff" "$fl"; then
                added="$added$entry$NL"
            else
                say "warning: could not write $ff" >&2
            fi
        fi
    fi
    if [ -n "$added" ]; then
        say "Added $dir to PATH with a line ending in '$MARKER' in:"
        printf '%s' "$added" | sed 's/^[a-z]*:[^:]*:/  /'
        say "Open a new terminal to use fastwarp, or run: . \"$HOME/.profile\""
        say "To undo: run the installer with --uninstall, or delete those lines."
    else
        say "The PATH line for $dir is already in your shell profiles; open a new terminal to use fastwarp."
    fi
}

print_path_help() {
    path_has "$dir" && return
    say "$dir is not on PATH, and the shell profiles were left unchanged."
    say "To use fastwarp, add it to PATH, for example:"
    say "  echo 'export PATH=\"$dir:\$PATH\"' >> ~/.bashrc   (bash; use ~/.zshrc for zsh)"
    say "  fish_add_path '$dir'   (fish)"
}

write_receipt() {
    new_receipt=$dir/.fastwarp-installer.new.$$
    if ! {
        printf '# Written by the fastwarp installer; --uninstall reads it.\n'
        printf 'dir %s\n' "$dir"
        printf '%s\n' "$1" | awk 'NF {print "sha256 " $0}'
        printf '%s' "$2" | awk 'NF && !seen[$0]++'
    } >"$new_receipt" || ! mv -f "$new_receipt" "$receipt"; then
        rm -f "$new_receipt"
        err "could not write $receipt"
    fi
}

resolve_dir() {
    if [ -n "$opt_dir" ]; then
        dir=$opt_dir
    elif [ -n "${FASTWARP_INSTALL_DIR:-}" ]; then
        dir=$FASTWARP_INSTALL_DIR
    elif [ -n "${XDG_BIN_HOME:-}" ]; then
        dir=$XDG_BIN_HOME
    else
        [ -n "${HOME:-}" ] || err "HOME is not set; give the folder with --dir"
        dir=$HOME/.local/bin
    fi
    case "$dir" in
        /*) ;;
        *) dir=$(pwd)/$dir ;;
    esac
    while [ "${dir%/}" != "$dir" ] && [ "$dir" != / ]; do
        dir=${dir%/}
    done
    case "$dir" in
        *:* | *"$NL"* | *\\*) err "the install folder must not contain ':', '\\' or a line break: $dir" ;;
    esac
    [ "$dir" != / ] || err "the install folder must not be /"
    bin=$dir/fastwarp
    receipt=$dir/.fastwarp-installer
}

expected_sha256() {
    [ "$version" = "$INSTALLER_VERSION" ] || return 0
    case "$target" in
        x86_64-unknown-linux-gnu) h=$SHA256_X86_64_LINUX ;;
        aarch64-unknown-linux-gnu) h=$SHA256_AARCH64_LINUX ;;
        aarch64-apple-darwin) h=$SHA256_AARCH64_MACOS ;;
        x86_64-apple-darwin) h=$SHA256_X86_64_MACOS ;;
        *) h= ;;
    esac
    case "$h" in
        *@*) ;;
        *) printf '%s\n' "$h" ;;
    esac
}

do_uninstall() {
    resolve_dir
    found=
    [ -f "$receipt" ] && found=1
    foreign=
    if [ -e "$bin" ] || [ -L "$bin" ]; then
        if is_owned; then
            rm -f "$bin"
            say "Removed $bin"
            found=1
        elif [ -n "$found" ]; then
            foreign=1
            say "warning: $bin was replaced since the install (for example by 'uv tool install'); it was left in place" >&2
        else
            err "$bin was not installed by this installer (for example a link from 'uv tool install', a pip script or a program you built); remove it with the tool that installed it"
        fi
    elif [ -z "$found" ]; then
        say "No fastwarp installed by this installer in $dir"
    fi
    removed=
    entries=$(receipt_entries)
    recorded=$(receipt_dir)
    if [ -n "$entries" ] && [ "$(canonical "$recorded")" != "$(canonical "$dir")" ]; then
        say "warning: the receipt in $dir was written for ${recorded:-another folder}; its PATH lines were left alone" >&2
        entries=
    fi
    old_ifs=$IFS
    IFS=$NL
    set -f
    for entry in $entries; do
        kind=${entry%%:*}
        rest=${entry#*:}
        line_dir=${rest%%:*}
        f=${rest#*:}
        case "$kind" in
            fish) line=$(fish_line "$line_dir") ;;
            *) line=$(posix_line "$line_dir") ;;
        esac
        if remove_line "$f" "$line"; then
            removed="$removed  $f$NL"
            [ "$kind" != fish ] || [ -s "$f" ] || rm -f "$f"
            if path_has "$line_dir"; then
                say "note: this terminal still has $line_dir on PATH; open a new terminal before installing again."
            fi
        fi
    done
    set +f
    IFS=$old_ifs
    rm -f "$receipt"
    for staged in "$dir"/.fastwarp.new.* "$dir"/.fastwarp-installer.new.*; do
        [ -f "$staged" ] && rm -f "$staged"
    done
    if [ -n "$removed" ]; then
        say "Removed the PATH lines for $dir from:"
        printf '%s' "$removed"
    fi
    if [ -n "$found" ] && [ -z "$foreign" ] && [ -d "$dir" ] && [ ! -L "$dir" ] && [ -z "$(ls -A "$dir" 2>/dev/null)" ]; then
        rmdir "$dir" 2>/dev/null || true
    fi
    if [ -n "$removed" ] && [ -d "$dir" ] && [ -n "$(ls -A "$dir" 2>/dev/null)" ]; then
        say "note: $dir still holds other files; the lines this installer wrote no longer put it on PATH."
    fi
}

do_install() {
    detect_target
    version=${opt_version:-${FASTWARP_VERSION:-$INSTALLER_VERSION}}
    version=${version#v}
    case "$version" in
        *@*) err "no version given, and this copy of the installer is not from a release; pass --version or set FASTWARP_VERSION" ;;
        *"$NL"*) err "invalid version: $version" ;;
    esac
    printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$' || err "invalid version: $version"
    resolve_dir
    check_owned "remove it first or choose another folder with --dir"

    archive=fastwarp-$target.tar.gz
    base=${FASTWARP_DOWNLOAD_URL:-$REPO_URL/releases/download/v$version}
    base=${base%/}
    pinned=$(expected_sha256)
    case "$base" in
        http://*@*) local_http=0 ;;
        http://127.0.0.1:* | http://127.0.0.1/* | http://localhost:* | http://localhost/*) local_http=1 ;;
        *) local_http=0 ;;
    esac
    case "$base" in
        http://*)
            if [ "$local_http" = 0 ]; then
                [ -n "$pinned" ] || err "refusing to download over plain HTTP without a checksum held in the installer: $base"
                say "warning: downloading over plain HTTP from $base" >&2
            fi
            ;;
        https://*)
            if ! have curl && [ -z "$pinned" ]; then
                err "curl is needed to install a version other than this installer's own: wget may follow a redirect from HTTPS to plain HTTP for the .sha256 file"
            fi
            ;;
    esac

    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t fastwarp)
    staged=
    trap 'rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"' EXIT
    trap 'rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"; exit 130' INT TERM

    say "Downloading fastwarp $version for $target from $base/$archive"
    download "$base/$archive" "$tmp/$archive" || err "download failed: $base/$archive"
    if [ -n "$pinned" ]; then
        expected=$pinned
        source_of_sum="the checksum held in this installer"
    else
        download "$base/$archive.sha256" "$tmp/$archive.sha256" || err "download failed: $base/$archive.sha256"
        expected=$(awk 'NR == 1 {print tolower($1)}' "$tmp/$archive.sha256")
        source_of_sum="$archive.sha256"
    fi
    printf '%s\n' "$expected" | grep -Eqx '[0-9a-f]{64}' || err "$source_of_sum is not a SHA-256 checksum"
    actual=$(sha256_of "$tmp/$archive" | tr 'A-F' 'a-f')
    if [ "$actual" != "$expected" ]; then
        err "SHA-256 checksum mismatch for $archive (expected $expected from $source_of_sum, got $actual); nothing was installed"
    fi
    say "Checksum OK ($source_of_sum): $actual"

    mkdir "$tmp/x"
    tar -xzf "$tmp/$archive" -C "$tmp/x" || err "could not extract $archive"
    src=$tmp/x/fastwarp-$target/fastwarp
    [ -f "$src" ] || err "$archive does not contain fastwarp-$target/fastwarp"

    mkdir -p "$dir" || err "could not create $dir"
    previous=$(receipt_entries)
    staged=$dir/.fastwarp.new.$$
    cp "$src" "$staged" || err "could not write to $dir"
    chmod 755 "$staged"
    if ! out=$("$staged" --version 2>&1); then
        rm -f "$staged"
        err "the downloaded fastwarp does not run on this system: $out"
    fi
    installed_sha=$(sha256_of "$staged" | tr 'A-F' 'a-f')
    old_shas=$(receipt_sha256s)
    write_receipt "$old_shas$NL$installed_sha" "$previous"
    mv -f "$staged" "$bin" || err "could not replace $bin"
    staged=
    write_receipt "$installed_sha" "$previous"
    say "Installed $out to $bin"

    added=
    if [ "$opt_no_modify_path" = 1 ]; then
        print_path_help
    else
        modify_path
    fi
    write_receipt "$installed_sha" "$previous$NL$added"

    other=$(command -v fastwarp 2>/dev/null || true)
    if [ -n "$other" ] && [ "$other" != "$bin" ]; then
        say "note: another fastwarp comes first on PATH: $other"
    fi
}

opt_version=
opt_dir=
case "${FASTWARP_NO_MODIFY_PATH:-}" in
    "" | 0) opt_no_modify_path=0 ;;
    *) opt_no_modify_path=1 ;;
esac
action=do_install
while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            [ $# -ge 2 ] || err "--version needs a value"
            opt_version=$2
            shift 2
            ;;
        --version=*)
            opt_version=${1#*=}
            shift
            ;;
        --dir)
            [ $# -ge 2 ] || err "--dir needs a value"
            opt_dir=$2
            shift 2
            ;;
        --dir=*)
            opt_dir=${1#*=}
            shift
            ;;
        --no-modify-path)
            opt_no_modify_path=1
            shift
            ;;
        --uninstall)
            action=do_uninstall
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            err "unknown option: $1 (see --help)"
            ;;
    esac
done

"$action"
